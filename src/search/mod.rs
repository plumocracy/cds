mod error;

use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::{Database, DirectoryClassification};
use crate::embed::Embedder;

pub use error::SearchError;

pub type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub path: String,
    pub score: f32,
}

pub struct Searcher<'a, E> {
    database: &'a Database,
    embedder: &'a E,
}

impl<'a, E> Searcher<'a, E>
where
    E: Embedder,
{
    pub fn new(database: &'a Database, embedder: &'a E) -> Self {
        Self { database, embedder }
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let query_embedding = self
            .embedder
            .embed(query)
            .map_err(|source| SearchError::EmbedQuery { source })?;
        let chunks = self
            .database
            .file_chunk_matches()
            .map_err(|source| SearchError::LoadDirectories { source })?;
        let temporal_filter = TemporalFilter::from_query(query);
        let use_deep_target = wants_deep_target(query);
        let query_terms = query_terms(query);

        let mut scores = HashMap::<String, f32>::new();
        for chunk in chunks {
            let mut score = cosine_similarity(&query_embedding, &chunk.embedding);
            if score <= 0.0 {
                continue;
            }

            let classifications = self
                .database
                .ancestor_classifications(&chunk.directory_path)
                .map_err(|source| SearchError::LoadDirectories { source })?;
            score += lexical_boost(
                &query_terms,
                [
                    chunk.content.as_str(),
                    chunk.file_name.as_str(),
                    chunk.directory_path.as_str(),
                ],
            );
            score += classification_boost(&query_terms, &classifications);
            score *= temporal_filter.score_multiplier(
                chunk
                    .file_modified_unix_seconds
                    .max(chunk.directory_modified_unix_seconds),
            );

            let target = if use_deep_target {
                chunk.directory_path
            } else {
                self.database
                    .general_indexed_directory(&chunk.directory_path)
                    .map_err(|source| SearchError::LoadDirectories { source })?
            };

            scores
                .entry(target)
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
        }

        for directory in self
            .database
            .directory_documents()
            .map_err(|source| SearchError::LoadDirectories { source })?
        {
            let classifications = self
                .database
                .directory_classifications(&directory.path)
                .map_err(|source| SearchError::LoadDirectories { source })?;
            let score = classification_boost(&query_terms, &classifications)
                + lexical_boost(
                    &query_terms,
                    [directory.path.as_str(), directory.name.as_str()],
                );
            if score <= 0.0 {
                continue;
            }

            let target = if use_deep_target {
                directory.path
            } else {
                self.database
                    .general_indexed_directory(&directory.path)
                    .map_err(|source| SearchError::LoadDirectories { source })?
            };

            scores
                .entry(target)
                .and_modify(|existing| *existing = existing.max(score))
                .or_insert(score);
        }

        let mut results = scores
            .into_iter()
            .filter_map(|(path, score)| (score > 0.0).then_some(SearchResult { path, score }))
            .collect::<Vec<_>>();

        results.sort_by(compare_results);
        results.truncate(limit);
        Ok(results)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TemporalFilter {
    min_modified_unix_seconds: Option<i64>,
}

impl TemporalFilter {
    fn from_query(query: &str) -> Self {
        let lower = query.to_ascii_lowercase();
        let seconds = unix_now();

        let min_modified_unix_seconds = if lower.contains("today") {
            Some(seconds - 24 * 60 * 60)
        } else if lower.contains("yesterday") {
            Some(seconds - 2 * 24 * 60 * 60)
        } else if lower.contains("last week") {
            Some(seconds - 14 * 24 * 60 * 60)
        } else if lower.contains("recent") || lower.contains("recently") {
            Some(seconds - 30 * 24 * 60 * 60)
        } else {
            None
        };

        Self {
            min_modified_unix_seconds,
        }
    }

    fn score_multiplier(self, modified_unix_seconds: i64) -> f32 {
        let Some(min_modified_unix_seconds) = self.min_modified_unix_seconds else {
            return 1.0;
        };

        if modified_unix_seconds >= min_modified_unix_seconds {
            1.35
        } else {
            0.25
        }
    }
}

fn wants_deep_target(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    [
        "migration",
        "migrations",
        "test",
        "tests",
        "spec",
        "schema",
        "schemas",
        "component",
        "components",
        "route",
        "routes",
        "controller",
        "controllers",
        "model",
        "models",
        "config",
    ]
    .iter()
    .any(|signal| lower.contains(signal))
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.len() > 2)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn lexical_boost<'a>(query_terms: &[String], haystacks: impl IntoIterator<Item = &'a str>) -> f32 {
    let haystack = haystacks
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("\n");

    query_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .count() as f32
        * 0.04
}

fn classification_boost(
    query_terms: &[String],
    classifications: &[DirectoryClassification],
) -> f32 {
    classifications
        .iter()
        .map(|classification| {
            let label_terms = query_terms_from_label(&classification.label);
            if label_terms.is_empty() {
                return 0.0;
            }

            let matched_terms = label_terms
                .iter()
                .filter(|term| query_terms.contains(term))
                .count();

            if matched_terms == label_terms.len() {
                classification.confidence * 1.2
            } else if matched_terms > 0 {
                classification.confidence * 0.25 * matched_terms as f32
            } else {
                0.0
            }
        })
        .sum()
}

fn query_terms_from_label(label: &str) -> Vec<String> {
    query_terms(label)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn compare_results(left: &SearchResult, right: &SearchResult) -> Ordering {
    right
        .score
        .partial_cmp(&left.score)
        .unwrap_or(Ordering::Equal)
        .then_with(|| left.path.cmp(&right.path))
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || right.is_empty() || left.len() != right.len() {
        return 0.0;
    }

    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;

    for (left, right) in left.iter().zip(right) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }

    if left_norm == 0.0 || right_norm == 0.0 {
        return 0.0;
    }

    dot / (left_norm.sqrt() * right_norm.sqrt())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::Settings;
    use crate::db::Database;
    use crate::embed::FakeEmbedder;
    use crate::index::Indexer;

    #[test]
    fn returns_best_directory_match() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let chrome = root.join("chrome-extension");
        let notes = root.join("meeting-notes");
        fs::create_dir_all(&chrome).unwrap();
        fs::create_dir_all(&notes).unwrap();
        fs::write(
            chrome.join("README.md"),
            "Chrome extension manifest browser popup",
        )
        .unwrap();
        fs::write(notes.join("README.md"), "Meeting notes calendar agenda").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("chrome extension", 3)
            .unwrap();

        assert_eq!(results.first().unwrap().path, chrome.to_string_lossy());
    }

    #[test]
    fn returns_directory_by_deterministic_type_classification() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let rust = root.join("cds-rust");
        let notes = root.join("notes");
        fs::create_dir_all(&rust).unwrap();
        fs::create_dir_all(&notes).unwrap();
        fs::write(rust.join("Cargo.toml"), "[package]\nname = \"cds\"").unwrap();
        fs::write(notes.join("README.md"), "rust colored notes without cargo").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("rust project", 3)
            .unwrap();

        assert_eq!(results.first().unwrap().path, rust.to_string_lossy());
    }

    #[test]
    fn prefers_deeper_directory_when_query_has_strong_signal() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let chrome = root.join("chrome-extension");
        let migrations = chrome.join("server/db/migrations");
        fs::create_dir_all(&migrations).unwrap();
        fs::write(
            migrations.join("001_create_popup_events.sql"),
            "create table popup_events for chrome extension telemetry migrations",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("migrations chrome extension", 3)
            .unwrap();

        assert_eq!(results.first().unwrap().path, migrations.to_string_lossy());
    }
}
