mod classify;
mod error;
mod file;
mod progress;
mod scanner;
mod summary;

use std::path::PathBuf;

use crate::config::Settings;
use crate::db::Database;
use crate::embed::Embedder;

pub use error::IndexError;
pub use progress::{IndexProgress, NoopProgress};
pub use scanner::IndexReport;

pub type Result<T> = std::result::Result<T, IndexError>;

pub struct Indexer<'a, E> {
    settings: &'a Settings,
    database: &'a Database,
    embedder: &'a E,
}

impl<'a, E> Indexer<'a, E>
where
    E: Embedder,
{
    pub fn new(settings: &'a Settings, database: &'a Database, embedder: &'a E) -> Self {
        Self {
            settings,
            database,
            embedder,
        }
    }

    pub async fn index_configured_roots(&self) -> Result<IndexReport> {
        let mut progress = NoopProgress;
        self.index_configured_roots_with_progress(&mut progress)
            .await
    }

    pub async fn index_configured_roots_with_progress<P>(
        &self,
        progress: &mut P,
    ) -> Result<IndexReport>
    where
        P: IndexProgress + ?Sized,
    {
        let roots = self
            .settings
            .expanded_roots()
            .map_err(|source| IndexError::ExpandConfiguredRoots { source })?;
        self.index_roots_with_progress(roots, progress).await
    }

    pub async fn index_roots(&self, roots: Vec<PathBuf>) -> Result<IndexReport> {
        let mut progress = NoopProgress;
        self.index_roots_with_progress(roots, &mut progress).await
    }

    pub async fn index_roots_with_progress<P>(
        &self,
        roots: Vec<PathBuf>,
        progress: &mut P,
    ) -> Result<IndexReport>
    where
        P: IndexProgress + ?Sized,
    {
        let mut report = IndexReport::default();

        for root in roots {
            if !root.exists() {
                report.roots_missing += 1;
                continue;
            }

            if !root.is_dir() {
                report.roots_not_directories += 1;
                continue;
            }

            if is_excluded_index_root(self.settings, &root) {
                self.database
                    .delete_path_tree(&root.to_string_lossy())
                    .await
                    .map_err(|source| IndexError::PruneExcludedPath {
                        path: root.to_string_lossy().into_owned(),
                        source: Box::new(source),
                    })?;
                report.entries_skipped += 1;
                continue;
            }

            report.roots_scanned += 1;
            scanner::scan_root_with_progress(
                &root,
                self.settings,
                self.database,
                self.embedder,
                &mut report,
                progress,
            )
            .await
            .map_err(|source| IndexError::ScanRoot {
                root: root.clone(),
                source: Box::new(source),
            })?;
        }

        Ok(report)
    }
}

fn is_excluded_index_root(settings: &Settings, root: &std::path::Path) -> bool {
    root.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| settings.index.is_excluded_directory_name(name))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::db::{Database, DocumentKind, IndexedDocument};
    use crate::embed::{Embedder, FakeEmbedder};

    #[tokio::test]
    async fn indexes_directory_summaries_and_skips_excluded_paths() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let app = root.join("chrome-extension");
        let ignored = app.join("node_modules");
        fs::create_dir_all(&ignored).unwrap();
        fs::write(app.join("README.md"), "Chrome extension manifest tools").unwrap();
        fs::write(
            app.join(".env.production"),
            "DATABASE_URL=postgres://secret",
        )
        .unwrap();
        fs::write(ignored.join("package.json"), "should not appear").unwrap();
        let asset_catalog = app.join("macos/Assets.xcassets/Custom Icon.appiconset");
        fs::create_dir_all(&asset_catalog).unwrap();
        fs::write(asset_catalog.join("Contents.json"), "should not be indexed").unwrap();
        let hidden = app.join(".vscode");
        fs::create_dir_all(&hidden).unwrap();
        fs::write(hidden.join("settings.json"), "should not be indexed").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        database
            .upsert_document(&IndexedDocument {
                path: asset_catalog.to_string_lossy().into_owned(),
                name: "Custom Icon.appiconset".to_string(),
                kind: DocumentKind::Directory,
                parent_path: asset_catalog
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned()),
                searchable_text: "stale custom icon asset catalog".to_string(),
                embedding: vec![1.0; 16],
                metadata_fingerprint: "stale".to_string(),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 0,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: 0,
            })
            .await
            .unwrap();
        database
            .upsert_document(&IndexedDocument {
                path: hidden.to_string_lossy().into_owned(),
                name: ".vscode".to_string(),
                kind: DocumentKind::Directory,
                parent_path: hidden
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned()),
                searchable_text: "stale hidden editor config".to_string(),
                embedding: vec![1.0; 16],
                metadata_fingerprint: "stale".to_string(),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 0,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: 0,
            })
            .await
            .unwrap();
        let embedder = FakeEmbedder::new(16);
        let indexer = Indexer::new(&settings, &database, &embedder);

        let report = indexer.index_roots(vec![root.clone()]).await.unwrap();

        assert_eq!(report.roots_scanned, 1);
        assert_eq!(report.directories_indexed, 3);
        assert_eq!(report.text_files_indexed, 1);
        assert_eq!(report.file_chunks_indexed, 1);
        assert_eq!(report.entries_skipped, 4);
        assert_eq!(database.document_count().await.unwrap(), 3);

        let app_document = database
            .get_document(&app.to_string_lossy())
            .await
            .unwrap()
            .expect("app directory is indexed");
        assert!(
            app_document
                .searchable_text
                .contains("Chrome extension manifest tools")
        );
        assert!(!app_document.searchable_text.contains("should not appear"));
        assert!(!app_document.searchable_text.contains("DATABASE_URL"));
        assert!(!app_document.searchable_text.contains(".vscode"));
        assert_eq!(
            database
                .get_document(&asset_catalog.to_string_lossy())
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            database
                .get_document(&hidden.to_string_lossy())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn batches_file_chunk_embeddings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("README.md"), "alpha").unwrap();
        fs::write(app.join("Cargo.toml"), "bravo").unwrap();
        fs::write(app.join("schema.sql"), "charlie").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = CountingEmbedder::new(16);
        let calls = Arc::clone(&embedder.document_batch_calls);
        let indexer = Indexer::new(&settings, &database, &embedder);

        let report = indexer.index_roots(vec![root]).await.unwrap();

        assert_eq!(report.text_files_indexed, 3);
        assert_eq!(report.file_chunks_indexed, 3);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skips_hidden_roots_and_prunes_stale_rows() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".hidden-project");
        fs::create_dir_all(&root).unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        database
            .upsert_document(&IndexedDocument {
                path: root.to_string_lossy().into_owned(),
                name: ".hidden-project".to_string(),
                kind: DocumentKind::Directory,
                parent_path: root
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned()),
                searchable_text: "stale hidden root".to_string(),
                embedding: vec![1.0; 16],
                metadata_fingerprint: "stale".to_string(),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 0,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: 0,
            })
            .await
            .unwrap();
        let embedder = FakeEmbedder::new(16);
        let indexer = Indexer::new(&settings, &database, &embedder);

        let report = indexer.index_roots(vec![root.clone()]).await.unwrap();

        assert_eq!(report.roots_scanned, 0);
        assert_eq!(report.entries_skipped, 1);
        assert_eq!(
            database
                .get_document(&root.to_string_lossy())
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn limits_recursion_depth_per_top_level_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let top_level = root.join("app");
        let depth_one = top_level.join("packages");
        let depth_two = depth_one.join("web");
        let depth_three = depth_two.join("src");
        let too_deep = depth_three.join("components");
        fs::create_dir_all(&too_deep).unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        database
            .upsert_document(&IndexedDocument {
                path: too_deep.to_string_lossy().into_owned(),
                name: "components".to_string(),
                kind: DocumentKind::Directory,
                parent_path: too_deep
                    .parent()
                    .map(|path| path.to_string_lossy().into_owned()),
                searchable_text: "stale deeply nested component directory".to_string(),
                embedding: vec![1.0; 16],
                metadata_fingerprint: "stale".to_string(),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 0,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: 0,
            })
            .await
            .unwrap();
        let embedder = FakeEmbedder::new(16);
        let indexer = Indexer::new(&settings, &database, &embedder);

        let report = indexer.index_roots(vec![root.clone()]).await.unwrap();

        assert_eq!(report.roots_scanned, 1);
        assert_eq!(report.directories_indexed, 5);
        assert_eq!(report.text_files_indexed, 0);
        assert_eq!(report.file_chunks_indexed, 0);
        assert_eq!(report.entries_skipped, 1);
        assert!(
            database
                .get_document(&root.to_string_lossy())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            database
                .get_document(&top_level.to_string_lossy())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            database
                .get_document(&depth_three.to_string_lossy())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            database
                .get_document(&too_deep.to_string_lossy())
                .await
                .unwrap(),
            None
        );
    }

    #[derive(Debug)]
    struct CountingEmbedder {
        dimensions: usize,
        document_batch_calls: Arc<AtomicUsize>,
    }

    impl CountingEmbedder {
        fn new(dimensions: usize) -> Self {
            Self {
                dimensions,
                document_batch_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Embedder for CountingEmbedder {
        fn dimensions(&self) -> usize {
            self.dimensions
        }

        fn embed(&self, _text: &str) -> crate::embed::Result<Vec<f32>> {
            Ok(vec![1.0; self.dimensions])
        }

        fn embed_documents(&self, texts: &[String]) -> crate::embed::Result<Vec<Vec<f32>>> {
            self.document_batch_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![vec![1.0; self.dimensions]; texts.len()])
        }
    }
}
