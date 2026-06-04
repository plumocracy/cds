use std::fs;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Mutex;

use super::{classify, file, summary};
use crate::config::Settings;
use crate::db::{Database, IndexedDocument};
use crate::embed::{EmbedError, Embedder};
use crate::index::{IndexError, IndexProgress, Result};

const FILE_CHUNK_EMBED_BATCH_SIZE: usize = 64;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct IndexReport {
    pub roots_scanned: u64,
    pub roots_missing: u64,
    pub roots_not_directories: u64,
    pub directories_indexed: u64,
    pub files_seen: u64,
    pub text_files_indexed: u64,
    pub file_chunks_indexed: u64,
    pub entries_skipped: u64,
}

impl IndexReport {
    pub fn human_summary(&self) -> String {
        format!(
            "indexed {} directories and {} text file chunks from {} text files across {} roots ({} files seen, {} skipped, {} missing roots, {} non-directory roots)",
            self.directories_indexed,
            self.file_chunks_indexed,
            self.text_files_indexed,
            self.roots_scanned,
            self.files_seen,
            self.entries_skipped,
            self.roots_missing,
            self.roots_not_directories,
        )
    }
}

pub async fn scan_root_with_progress<E, P>(
    root: &Path,
    settings: &Settings,
    database: &Database,
    embedder: &E,
    report: &mut IndexReport,
    progress: &mut P,
) -> Result<()>
where
    E: Embedder,
    P: IndexProgress + ?Sized,
{
    let batch = FileIndexBatch::new(database, embedder);
    scan_directory(
        root,
        settings,
        &batch,
        report,
        progress,
        DepthPosition::IndexRoot,
    )
    .await?;
    batch.flush().await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepthPosition {
    IndexRoot,
    TopLevelDirectory { depth: usize },
}

impl DepthPosition {
    fn child_position(self) -> Option<Self> {
        match self {
            Self::IndexRoot => Some(Self::TopLevelDirectory { depth: 0 }),
            Self::TopLevelDirectory { depth } => depth
                .checked_add(1)
                .map(|depth| Self::TopLevelDirectory { depth }),
        }
    }
}

fn scan_directory<'a, E, P>(
    directory: &'a Path,
    settings: &'a Settings,
    batch: &'a FileIndexBatch<'a, E>,
    report: &'a mut IndexReport,
    progress: &'a mut P,
    depth_position: DepthPosition,
) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>
where
    E: Embedder + 'a,
    P: IndexProgress + ?Sized + 'a,
{
    Box::pin(async move {
        progress.directory_started(directory);

        let document = summary::summarize_directory(directory, settings).map_err(|source| {
            IndexError::SummarizeDirectory {
                path: directory.to_path_buf(),
                source: Box::new(source),
            }
        })?;

        batch
            .database
            .upsert_document(&document)
            .await
            .map_err(|source| IndexError::StoreDocument {
                path: document.path.clone(),
                source: Box::new(source),
            })?;
        let classifications = classify::classify_directory(directory, settings)?;
        batch
            .database
            .replace_directory_classifications(&document.path, &classifications)
            .await
            .map_err(|source| IndexError::StoreDirectoryClassifications {
                path: document.path.clone(),
                source: Box::new(source),
            })?;
        report.directories_indexed += 1;

        let entries = fs::read_dir(directory).map_err(|source| IndexError::ReadDirectory {
            path: directory.to_path_buf(),
            source,
        })?;

        for entry in entries {
            let entry = entry.map_err(|source| IndexError::ReadDirectoryEntry {
                path: directory.to_path_buf(),
                source,
            })?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();

            let file_type = entry
                .file_type()
                .map_err(|source| IndexError::ReadFileType {
                    path: path.clone(),
                    source,
                })?;

            if file_type.is_dir() && settings.index.is_excluded_directory_name(&name) {
                batch
                    .database
                    .delete_path_tree(&path.to_string_lossy())
                    .await
                    .map_err(|source| IndexError::PruneExcludedPath {
                        path: path.to_string_lossy().into_owned(),
                        source: Box::new(source),
                    })?;
                report.entries_skipped += 1;
                continue;
            }

            if file_type.is_dir() {
                let Some(child_position) = depth_position.child_position() else {
                    prune_unindexed_directory(batch.database, report, &path).await?;
                    continue;
                };

                if exceeds_max_depth(settings, child_position) {
                    prune_unindexed_directory(batch.database, report, &path).await?;
                    continue;
                }

                scan_directory(&path, settings, batch, report, progress, child_position).await?;
            } else if file_type.is_file() {
                if settings.index.is_excluded_name(&name) {
                    report.entries_skipped += 1;
                    continue;
                }

                report.files_seen += 1;
                if let Some(indexed_file) = file::prepare_text_file(&path, directory, settings)? {
                    report.text_files_indexed += 1;
                    report.file_chunks_indexed +=
                        u64::try_from(indexed_file.chunks.len()).unwrap_or(u64::MAX);
                    batch.push(indexed_file).await?;
                }
            } else {
                report.entries_skipped += 1;
            }
        }

        Ok(())
    })
}

fn exceeds_max_depth(settings: &Settings, depth_position: DepthPosition) -> bool {
    match depth_position {
        DepthPosition::IndexRoot => false,
        DepthPosition::TopLevelDirectory { depth } => {
            depth > settings.index.max_depth_per_top_level_directory
        }
    }
}

async fn prune_unindexed_directory(
    database: &Database,
    report: &mut IndexReport,
    path: &Path,
) -> Result<()> {
    database
        .delete_path_tree(&path.to_string_lossy())
        .await
        .map_err(|source| IndexError::PruneExcludedPath {
            path: path.to_string_lossy().into_owned(),
            source: Box::new(source),
        })?;
    report.entries_skipped += 1;
    Ok(())
}

struct FileIndexBatch<'a, E> {
    database: &'a Database,
    embedder: &'a E,
    state: Mutex<FileIndexBatchState>,
}

#[derive(Debug, Default)]
struct FileIndexBatchState {
    files: Vec<file::PreparedIndexedFileData>,
    chunk_count: usize,
}

impl<'a, E> FileIndexBatch<'a, E>
where
    E: Embedder,
{
    fn new(database: &'a Database, embedder: &'a E) -> Self {
        Self {
            database,
            embedder,
            state: Mutex::new(FileIndexBatchState::default()),
        }
    }

    async fn push(&self, file: file::PreparedIndexedFileData) -> Result<()> {
        let should_flush = {
            let mut state = self
                .state
                .lock()
                .expect("file index batch lock is poisoned");
            state.chunk_count = state.chunk_count.saturating_add(file.chunks.len());
            state.files.push(file);
            state.chunk_count >= FILE_CHUNK_EMBED_BATCH_SIZE
        };

        if should_flush {
            self.flush().await?;
        }

        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        let files = {
            let mut state = self
                .state
                .lock()
                .expect("file index batch lock is poisoned");
            if state.files.is_empty() {
                return Ok(());
            }
            state.chunk_count = 0;
            std::mem::take(&mut state.files)
        };
        let first_path = files
            .first()
            .map(|file| file.file.path.clone())
            .unwrap_or_else(|| "<batch>".to_string());
        let texts = files
            .iter()
            .flat_map(|file| file.chunks.iter().map(|chunk| chunk.content.clone()))
            .collect::<Vec<_>>();

        let embeddings =
            self.embedder
                .embed_documents(&texts)
                .map_err(|source| IndexError::EmbedSummary {
                    path: first_path.clone().into(),
                    source,
                })?;

        if embeddings.len() != texts.len() {
            return Err(IndexError::EmbedSummary {
                path: first_path.into(),
                source: EmbedError::Model {
                    message: format!(
                        "embedding model returned {} vectors for {} file chunks",
                        embeddings.len(),
                        texts.len()
                    ),
                },
            });
        }

        let mut embeddings = embeddings.into_iter();
        let indexed_files = files
            .into_iter()
            .map(|file| file.into_indexed(&mut embeddings))
            .collect::<Vec<_>>();
        let file_refs = indexed_files
            .iter()
            .map(|file| (&file.file, file.chunks.as_slice()))
            .collect::<Vec<_>>();

        self.database
            .upsert_files_with_chunks(&file_refs)
            .await
            .map_err(|source| IndexError::StoreFileChunks {
                path: first_path,
                source: Box::new(source),
            })?;

        Ok(())
    }
}

#[allow(dead_code)]
fn _assert_document_is_send(document: IndexedDocument) -> IndexedDocument {
    document
}
