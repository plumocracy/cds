use std::fs;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use super::{classify, file, summary};
use crate::config::Settings;
use crate::db::{Database, IndexedDocument};
use crate::embed::Embedder;
use crate::index::{IndexError, IndexProgress, Result};

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
    scan_directory(
        root,
        settings,
        database,
        embedder,
        report,
        progress,
        DepthPosition::IndexRoot,
    )
    .await
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
    database: &'a Database,
    embedder: &'a E,
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

        database
            .upsert_document(&document)
            .await
            .map_err(|source| IndexError::StoreDocument {
                path: document.path.clone(),
                source: Box::new(source),
            })?;
        let classifications = classify::classify_directory(directory, settings)?;
        database
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
                database
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
                    prune_unindexed_directory(database, report, &path).await?;
                    continue;
                };

                if exceeds_max_depth(settings, child_position) {
                    prune_unindexed_directory(database, report, &path).await?;
                    continue;
                }

                scan_directory(
                    &path,
                    settings,
                    database,
                    embedder,
                    report,
                    progress,
                    child_position,
                )
                .await?;
            } else if file_type.is_file() {
                if settings.index.is_excluded_name(&name) {
                    report.entries_skipped += 1;
                    continue;
                }

                report.files_seen += 1;
                if let Some(indexed_file) =
                    file::index_text_file(&path, directory, settings, embedder)?
                {
                    report.text_files_indexed += 1;
                    report.file_chunks_indexed +=
                        u64::try_from(indexed_file.chunks.len()).unwrap_or(u64::MAX);
                    database
                        .upsert_file(&indexed_file.file)
                        .await
                        .map_err(|source| IndexError::StoreFile {
                            path: indexed_file.file.path.clone(),
                            source: Box::new(source),
                        })?;
                    database
                        .replace_file_chunks(&indexed_file.file.path, &indexed_file.chunks)
                        .await
                        .map_err(|source| IndexError::StoreFileChunks {
                            path: indexed_file.file.path,
                            source: Box::new(source),
                        })?;
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

#[allow(dead_code)]
fn _assert_document_is_send(document: IndexedDocument) -> IndexedDocument {
    document
}
