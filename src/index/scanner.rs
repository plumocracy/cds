use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

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

    fn merge(&mut self, other: Self) {
        self.roots_scanned += other.roots_scanned;
        self.roots_missing += other.roots_missing;
        self.roots_not_directories += other.roots_not_directories;
        self.directories_indexed += other.directories_indexed;
        self.files_seen += other.files_seen;
        self.text_files_indexed += other.text_files_indexed;
        self.file_chunks_indexed += other.file_chunks_indexed;
        self.entries_skipped += other.entries_skipped;
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
    let mut scan = scan_root_concurrently(root, settings)?;
    scan.directories
        .sort_by(|left, right| left.document.path.cmp(&right.document.path));
    scan.files
        .sort_by(|left, right| left.file.path.cmp(&right.file.path));
    scan.pruned_paths.sort();
    scan.pruned_paths.dedup();

    for path in &scan.pruned_paths {
        database
            .delete_path_tree(path)
            .await
            .map_err(|source| IndexError::PruneExcludedPath {
                path: path.clone(),
                source: Box::new(source),
            })?;
    }

    for directory in &scan.directories {
        progress.directory_started(Path::new(&directory.document.path));
    }

    let directory_refs = scan
        .directories
        .iter()
        .map(|directory| (&directory.document, directory.classifications.as_slice()))
        .collect::<Vec<_>>();
    database
        .upsert_directories_with_classifications(&directory_refs)
        .await
        .map_err(|source| IndexError::StoreDocument {
            path: root.to_string_lossy().into_owned(),
            source: Box::new(source),
        })?;

    let batch = FileIndexBatch::new(database, embedder);
    for file in scan.files {
        batch.push(file).await?;
    }
    batch.flush().await?;

    report.merge(scan.report);
    Ok(())
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

fn exceeds_max_depth(settings: &Settings, depth_position: DepthPosition) -> bool {
    match depth_position {
        DepthPosition::IndexRoot => false,
        DepthPosition::TopLevelDirectory { depth } => {
            depth > settings.index.max_depth_per_top_level_directory
        }
    }
}

#[derive(Debug, Clone)]
struct ScanJob {
    directory: PathBuf,
    depth_position: DepthPosition,
}

#[derive(Debug, Default)]
struct ConcurrentScan {
    directories: Vec<ScannedDirectory>,
    files: Vec<file::PreparedIndexedFileData>,
    pruned_paths: Vec<String>,
    report: IndexReport,
}

#[derive(Debug)]
struct ScannedDirectory {
    document: IndexedDocument,
    classifications: Vec<crate::db::DirectoryClassification>,
}

#[derive(Debug)]
struct ScanOutput {
    directory: ScannedDirectory,
    files: Vec<file::PreparedIndexedFileData>,
    child_jobs: Vec<ScanJob>,
    pruned_paths: Vec<String>,
    report: IndexReport,
}

struct ScanQueue {
    state: Mutex<ScanQueueState>,
    changed: Condvar,
}

#[derive(Debug)]
struct ScanQueueState {
    pending: VecDeque<ScanJob>,
    active: usize,
    stopped: bool,
}

fn scan_root_concurrently(root: &Path, settings: &Settings) -> Result<ConcurrentScan> {
    let worker_count = scan_worker_count();
    let queue = Arc::new(ScanQueue {
        state: Mutex::new(ScanQueueState {
            pending: VecDeque::from([ScanJob {
                directory: root.to_path_buf(),
                depth_position: DepthPosition::IndexRoot,
            }]),
            active: 0,
            stopped: false,
        }),
        changed: Condvar::new(),
    });
    let settings = Arc::new(settings.clone());
    let scan = Arc::new(Mutex::new(ConcurrentScan::default()));
    let error = Arc::new(Mutex::new(None));

    thread::scope(|scope| {
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let settings = Arc::clone(&settings);
            let scan = Arc::clone(&scan);
            let error = Arc::clone(&error);

            scope.spawn(move || worker_loop(queue, settings, scan, error));
        }
    });

    if let Some(error) = error.lock().expect("scan error lock is poisoned").take() {
        return Err(error);
    }

    let scan = Arc::try_unwrap(scan)
        .expect("scan workers have stopped")
        .into_inner()
        .expect("scan result lock is poisoned");
    Ok(scan)
}

fn worker_loop(
    queue: Arc<ScanQueue>,
    settings: Arc<Settings>,
    scan: Arc<Mutex<ConcurrentScan>>,
    error: Arc<Mutex<Option<IndexError>>>,
) {
    loop {
        let Some(job) = next_scan_job(&queue) else {
            return;
        };

        let result = scan_directory_job(&job, &settings);
        match result {
            Ok(output) => finish_scan_job(&queue, &scan, output),
            Err(source) => {
                *error.lock().expect("scan error lock is poisoned") = Some(source);
                stop_scan_workers(&queue);
                return;
            }
        }
    }
}

fn next_scan_job(queue: &ScanQueue) -> Option<ScanJob> {
    let mut state = queue.state.lock().expect("scan queue lock is poisoned");

    loop {
        if state.stopped {
            return None;
        }

        if let Some(job) = state.pending.pop_front() {
            state.active += 1;
            return Some(job);
        }

        if state.active == 0 {
            state.stopped = true;
            queue.changed.notify_all();
            return None;
        }

        state = queue
            .changed
            .wait(state)
            .expect("scan queue lock is poisoned");
    }
}

fn finish_scan_job(queue: &ScanQueue, scan: &Mutex<ConcurrentScan>, output: ScanOutput) {
    {
        let mut scan = scan.lock().expect("scan result lock is poisoned");
        scan.directories.push(output.directory);
        scan.files.extend(output.files);
        scan.pruned_paths.extend(output.pruned_paths);
        scan.report.merge(output.report);
    }

    let mut state = queue.state.lock().expect("scan queue lock is poisoned");
    state.pending.extend(output.child_jobs);
    state.active = state.active.saturating_sub(1);
    queue.changed.notify_all();
}

fn stop_scan_workers(queue: &ScanQueue) {
    let mut state = queue.state.lock().expect("scan queue lock is poisoned");
    state.stopped = true;
    state.pending.clear();
    queue.changed.notify_all();
}

fn scan_directory_job(job: &ScanJob, settings: &Settings) -> Result<ScanOutput> {
    let document = summary::summarize_directory(&job.directory, settings).map_err(|source| {
        IndexError::SummarizeDirectory {
            path: job.directory.clone(),
            source: Box::new(source),
        }
    })?;
    let classifications = classify::classify_directory(&job.directory, settings)?;
    let mut output = ScanOutput {
        directory: ScannedDirectory {
            document,
            classifications,
        },
        files: Vec::new(),
        child_jobs: Vec::new(),
        pruned_paths: Vec::new(),
        report: IndexReport {
            directories_indexed: 1,
            ..IndexReport::default()
        },
    };

    let mut entries = fs::read_dir(&job.directory)
        .map_err(|source| IndexError::ReadDirectory {
            path: job.directory.clone(),
            source,
        })?
        .map(|entry| {
            let entry = entry.map_err(|source| IndexError::ReadDirectoryEntry {
                path: job.directory.clone(),
                source,
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry
                .file_type()
                .map_err(|source| IndexError::ReadFileType {
                    path: path.clone(),
                    source,
                })?;
            Ok((path, name, file_type))
        })
        .collect::<Result<Vec<_>>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    for (path, name, file_type) in entries {
        if file_type.is_dir() && settings.index.is_excluded_directory_name(&name) {
            output
                .pruned_paths
                .push(path.to_string_lossy().into_owned());
            output.report.entries_skipped += 1;
            continue;
        }

        if file_type.is_dir() {
            let Some(child_position) = job.depth_position.child_position() else {
                output
                    .pruned_paths
                    .push(path.to_string_lossy().into_owned());
                output.report.entries_skipped += 1;
                continue;
            };

            if exceeds_max_depth(settings, child_position) {
                output
                    .pruned_paths
                    .push(path.to_string_lossy().into_owned());
                output.report.entries_skipped += 1;
                continue;
            }

            output.child_jobs.push(ScanJob {
                directory: path,
                depth_position: child_position,
            });
        } else if file_type.is_file() {
            if settings.index.is_excluded_name(&name) {
                output.report.entries_skipped += 1;
                continue;
            }

            output.report.files_seen += 1;
            if let Some(indexed_file) = file::prepare_text_file(&path, &job.directory, settings)? {
                output.report.text_files_indexed += 1;
                output.report.file_chunks_indexed +=
                    u64::try_from(indexed_file.chunks.len()).unwrap_or(u64::MAX);
                output.files.push(indexed_file);
            }
        } else {
            output.report.entries_skipped += 1;
        }
    }

    Ok(output)
}

fn scan_worker_count() -> usize {
    thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(4)
        .clamp(2, 8)
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
