mod error;

use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{AppPaths, Settings, expand_tilde};
use crate::db::{Database, DirectoryTypeCount};
use crate::embed::{BgeSmallEmbedder, Embedder, FakeEmbedder};
use crate::error::{Result, app_err, config_err, embed_err};
use crate::index::{IndexProgress, IndexReport, Indexer, NoopProgress};
use crate::search::{SearchResult, Searcher};

pub use error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    pub config_file: PathBuf,
    pub database_file: PathBuf,
    pub index: IndexReport,
}

pub async fn init() -> Result<InitReport> {
    let mut progress = NoopProgress;
    init_with_progress(&mut progress).await
}

pub async fn init_with_progress<P>(progress: &mut P) -> Result<InitReport>
where
    P: IndexProgress + ?Sized,
{
    let paths = AppPaths::discover()?;
    let settings = Settings::load_or_create(&paths.config_file)?;
    let database = Database::open(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let indexer = Indexer::new(&settings, &database, &embedder);
    let index = indexer
        .index_configured_roots_with_progress(progress)
        .await?;

    Ok(InitReport {
        config_file: paths.config_file,
        database_file: paths.database_file,
        index,
    })
}

pub async fn index(roots: Vec<OsString>) -> Result<IndexReport> {
    let mut progress = NoopProgress;
    index_with_progress(roots, &mut progress).await
}

pub async fn index_with_progress<P>(roots: Vec<OsString>, progress: &mut P) -> Result<IndexReport>
where
    P: IndexProgress + ?Sized,
{
    let paths = AppPaths::discover()?;
    let settings = Settings::load_or_create(&paths.config_file)?;
    let database = Database::open(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let indexer = Indexer::new(&settings, &database, &embedder);

    if roots.is_empty() {
        return Ok(indexer
            .index_configured_roots_with_progress(progress)
            .await?);
    }

    let roots = roots
        .iter()
        .map(|root| {
            let root = root
                .to_str()
                .ok_or_else(|| app_err(AppError::InvalidIndexRootUtf8 { root: root.clone() }))?;
            expand_tilde(root).map_err(config_err)
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(indexer.index_roots_with_progress(roots, progress).await?)
}

pub async fn search(query: Vec<OsString>, limit: usize) -> Result<Vec<SearchResult>> {
    let query = join_query(query)?;
    search_text(&query, limit).await
}

pub async fn search_text(query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    let paths = AppPaths::discover()?;
    let database = Database::open_existing(&paths.database_file).await?;
    let embedder = RuntimeEmbedder::load(&paths)?;
    let searcher = Searcher::new(&database, &embedder);
    Ok(searcher.search(query, limit).await?)
}

pub async fn directory_type_counts() -> Result<Vec<DirectoryTypeCount>> {
    let paths = AppPaths::discover()?;
    let database = Database::open_existing(&paths.database_file).await?;
    Ok(database.directory_type_counts().await?)
}

pub async fn reset_database() -> Result<()> {
    let paths = AppPaths::discover()?;
    let database = Database::open_existing(&paths.database_file).await?;
    Ok(database.reset().await?)
}

pub async fn resolve_cd_script(args: Vec<OsString>) -> Vec<u8> {
    if let Some(query) = implied_search_query(&args)
        && let Ok(results) = search_text(&query, 5).await
        && let Some(result) = results
            .into_iter()
            .find(|result| Path::new(&result.path).is_dir())
    {
        return crate::emit_cd_script(&[OsString::from(result.path)]);
    }

    crate::emit_cd_script(&args)
}

pub fn implied_search_query(args: &[OsString]) -> Option<String> {
    semantic_query(args)
}

fn join_query(query: Vec<OsString>) -> Result<String> {
    let mut parts = Vec::with_capacity(query.len());

    for part in query {
        let part = part.to_str().ok_or_else(|| {
            app_err(AppError::InvalidSearchQueryUtf8 {
                query: part.clone(),
            })
        })?;
        parts.push(part.to_string());
    }

    Ok(parts.join(" "))
}

fn semantic_query(args: &[OsString]) -> Option<String> {
    semantic_query_in(args, current_shell_directory().as_deref())
}

fn semantic_query_in(args: &[OsString], directory: Option<&Path>) -> Option<String> {
    if args.is_empty() {
        return None;
    }

    let mut parts = Vec::with_capacity(args.len());
    for arg in args {
        let arg = arg.to_str()?;
        if is_cd_syntax(arg) {
            return None;
        }
        parts.push(arg);
    }

    let query = parts.join(" ");
    let first = query.chars().find(|ch| !ch.is_whitespace())?;

    if local_directory_starts_with(first, directory) {
        return None;
    }

    Some(query)
}

fn is_cd_syntax(arg: &str) -> bool {
    arg.is_empty()
        || arg == "-"
        || arg == "--"
        || arg == "."
        || arg == ".."
        || arg == "~"
        || arg.starts_with('-')
        || arg.starts_with("~/")
        || arg.contains('/')
}

fn local_directory_starts_with(first: char, directory: Option<&Path>) -> bool {
    let Some(directory) = directory else {
        return true;
    };

    let Ok(entries) = fs::read_dir(directory) else {
        return true;
    };

    let first = first.to_lowercase().to_string();

    entries.filter_map(std::result::Result::ok).any(|entry| {
        if !entry.path().is_dir() {
            return false;
        }

        entry
            .file_name()
            .to_string_lossy()
            .to_lowercase()
            .starts_with(&first)
    })
}

fn current_shell_directory() -> Option<PathBuf> {
    env::var_os("PWD")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| env::current_dir().ok())
}

enum RuntimeEmbedder {
    Bge(Box<BgeSmallEmbedder>),
    Fake(FakeEmbedder),
}

impl RuntimeEmbedder {
    fn load(paths: &AppPaths) -> Result<Self> {
        if env::var("CDS_EMBEDDER").is_ok_and(|value| value.eq_ignore_ascii_case("fake")) {
            return Ok(Self::Fake(FakeEmbedder::default()));
        }

        BgeSmallEmbedder::new(&paths.cache_dir)
            .map(Box::new)
            .map(Self::Bge)
            .map_err(embed_err)
    }
}

impl Embedder for RuntimeEmbedder {
    fn dimensions(&self) -> usize {
        match self {
            Self::Bge(embedder) => embedder.dimensions(),
            Self::Fake(embedder) => embedder.dimensions(),
        }
    }

    fn embed(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            Self::Bge(embedder) => embedder.embed(text),
            Self::Fake(embedder) => embedder.embed(text),
        }
    }

    fn embed_document(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            Self::Bge(embedder) => embedder.embed_document(text),
            Self::Fake(embedder) => embedder.embed_document(text),
        }
    }

    fn embed_query(&self, text: &str) -> crate::embed::Result<Vec<f32>> {
        match self {
            Self::Bge(embedder) => embedder.embed_query(text),
            Self::Fake(embedder) => embedder.embed_query(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(value: &str) -> OsString {
        OsString::from(value)
    }

    #[test]
    fn semantic_query_is_allowed_when_no_local_directory_prefix_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();

        assert_eq!(
            semantic_query_in(&[os("Projects")], Some(temp.path())),
            Some("Projects".to_string())
        );
    }

    #[test]
    fn semantic_query_is_blocked_when_local_directory_prefix_matches() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("playground")).unwrap();

        assert_eq!(
            semantic_query_in(&[os("Projects")], Some(temp.path())),
            None
        );
    }

    #[test]
    fn semantic_query_is_blocked_for_cd_syntax() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            semantic_query_in(&[os("-P"), os("Projects")], Some(temp.path())),
            None
        );
        assert_eq!(
            semantic_query_in(&[os("--"), os("-dir")], Some(temp.path())),
            None
        );
        assert_eq!(
            semantic_query_in(&[os("../Projects")], Some(temp.path())),
            None
        );
        assert_eq!(semantic_query_in(&[os("~")], Some(temp.path())), None);
    }

    #[test]
    fn semantic_query_joins_plain_words() {
        let temp = tempfile::tempdir().unwrap();

        assert_eq!(
            semantic_query_in(&[os("chrome"), os("extension")], Some(temp.path())),
            Some("chrome extension".to_string())
        );
    }

    #[test]
    fn implied_search_query_uses_cd_semantic_rules() {
        assert_eq!(implied_search_query(&[os("-P"), os("Projects")]), None);
        assert_eq!(implied_search_query(&[os("../Projects")]), None);
        assert_eq!(implied_search_query(&[]), None);
    }
}
