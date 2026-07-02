mod error;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use crate::config::{IndexSettings, Settings};
use crate::db::{
    Database, DirectoryClassification, FileChunkMatch, IndexedDocument, ModifiedTimeRange,
};
use crate::embed::Embedder;
use chrono::{DateTime, Datelike, Duration, Local, LocalResult, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

pub use error::SearchError;

pub type Result<T> = std::result::Result<T, SearchError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub path: String,
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchDryRun {
    pub query: String,
    pub temporal: TemporalDryRun,
    pub cache: CacheDryRun,
    pub candidate_terms: Vec<String>,
    pub sql_candidate_directories: Vec<String>,
    pub fuzzy_candidate_directories: Vec<String>,
    pub embedding_scores: Vec<EmbeddingScore>,
    pub results: Vec<SearchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheDryRun {
    pub status: CacheDryRunStatus,
    pub directory_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CacheDryRunStatus {
    Hit,
    Miss,
    NotUsed,
}

impl CacheDryRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::NotUsed => "not used",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemporalDryRun {
    pub cleaned_query: String,
    pub semantic_query: String,
    pub matched_phrase: Option<String>,
    pub start_unix_seconds: Option<i64>,
    pub end_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingScore {
    pub directory_path: String,
    pub file_path: String,
    pub file_name: String,
    pub is_current: bool,
    pub cosine_score: f32,
    pub content_preview: String,
}

pub struct Searcher<'a, E> {
    database: &'a Database,
    embedder: &'a E,
    index_settings: IndexSettings,
    generic_terms: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct SearchCache {
    generic_terms: HashSet<String>,
    index_settings: IndexSettings,
    revision: i64,
    directories: Vec<IndexedDocument>,
    path_term_stats: PathTermStats,
}

impl SearchCache {
    pub async fn load(
        database: &Database,
        generic_terms: &HashSet<String>,
        index_settings: &IndexSettings,
    ) -> Result<Self> {
        loop {
            let revision = database
                .current_revision()
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?;
            let directories = database
                .directory_search_documents()
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?;
            let directories = filter_indexable_directories(directories, index_settings);
            let current_revision = database
                .current_revision()
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?;
            if revision != current_revision {
                continue;
            }

            let path_term_stats = PathTermStats::from_directories(&directories, generic_terms);

            return Ok(Self {
                generic_terms: generic_terms.clone(),
                index_settings: index_settings.clone(),
                revision,
                directories,
                path_term_stats,
            });
        }
    }

    pub fn matches_revision_and_settings(
        &self,
        revision: i64,
        generic_terms: &HashSet<String>,
        index_settings: &IndexSettings,
    ) -> bool {
        self.revision == revision
            && &self.generic_terms == generic_terms
            && &self.index_settings == index_settings
    }
}

impl<'a, E> Searcher<'a, E>
where
    E: Embedder,
{
    pub fn new(database: &'a Database, embedder: &'a E) -> Self {
        let index_settings = IndexSettings::default();
        let generic_terms = index_settings.generic_terms.clone();
        Self::new_with_index_settings(database, embedder, index_settings, generic_terms)
    }

    pub fn new_with_settings(database: &'a Database, embedder: &'a E, settings: &Settings) -> Self {
        Self::new_with_index_settings(
            database,
            embedder,
            settings.index.clone(),
            settings.index.generic_terms.clone(),
        )
    }

    pub fn new_with_generic_terms(
        database: &'a Database,
        embedder: &'a E,
        generic_terms: impl IntoIterator<Item = String>,
    ) -> Self {
        let generic_terms = generic_terms.into_iter().collect::<Vec<_>>();
        let index_settings = IndexSettings {
            generic_terms: generic_terms.clone(),
            ..IndexSettings::default()
        };
        Self::new_with_index_settings(database, embedder, index_settings, generic_terms)
    }

    fn new_with_index_settings(
        database: &'a Database,
        embedder: &'a E,
        index_settings: IndexSettings,
        generic_terms: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            database,
            embedder,
            index_settings,
            generic_terms: generic_terms
                .into_iter()
                .map(|term| term.to_ascii_lowercase())
                .collect(),
        }
    }

    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        self.search_with_temporal_query(TemporalQuery::from_query(query), limit, None)
            .await
    }

    pub async fn search_with_cache(
        &self,
        query: &str,
        limit: usize,
        cache: &SearchCache,
    ) -> Result<Vec<SearchResult>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        self.search_with_temporal_query(TemporalQuery::from_query(query), limit, Some(cache))
            .await
    }

    async fn search_with_temporal_query(
        &self,
        temporal_query: TemporalQuery,
        limit: usize,
        cache: Option<&SearchCache>,
    ) -> Result<Vec<SearchResult>> {
        let temporal_filter = temporal_query.filter;
        let search_query = temporal_query.search_query();
        let query_terms = query_terms(search_query);
        let path_terms = path_query_terms(search_query);
        let loaded_cache;
        let cache = if let Some(cache) = cache {
            cache
        } else {
            loaded_cache =
                SearchCache::load(self.database, &self.generic_terms, &self.index_settings).await?;
            &loaded_cache
        };
        let all_directories = &cache.directories;
        let path_term_stats = &cache.path_term_stats;
        let candidate_terms = candidate_filter_terms(&path_terms, &self.generic_terms);
        let mut candidate_directories = if candidate_terms.is_empty() {
            Vec::new()
        } else {
            self.database
                .directory_search_candidates_by_terms(&candidate_terms)
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?
                .into_iter()
                .filter(|directory| {
                    is_indexable_directory_path(&directory.path, &self.index_settings)
                })
                .collect()
        };
        add_fuzzy_directory_candidates(
            &mut candidate_directories,
            all_directories,
            &candidate_terms,
        );
        if !temporal_filter.has_range()
            && let Some(directory) =
                high_confidence_directory_candidate(&candidate_directories, &candidate_terms)
        {
            let mut fast_target_cache = HashMap::new();
            let target = navigation_target(
                self.database,
                &mut fast_target_cache,
                &directory.path,
                &path_terms,
                &self.generic_terms,
            )
            .await?;
            let path_match = path_match_boost(
                &path_terms,
                &directory.path,
                Some(&directory.name),
                path_term_stats,
            );

            return Ok(vec![SearchResult {
                path: target,
                score: 10.0 + path_match.score,
            }]);
        }

        let query_embedding = self
            .embedder
            .embed_query(search_query)
            .map_err(|source| SearchError::EmbedQuery { source })?;
        let (chunks, directories) = if candidate_directories.is_empty() {
            let chunks = self
                .database
                .nearest_file_chunk_matches_with_modified_range(
                    &query_embedding,
                    temporal_filter.modified_range(),
                    semantic_candidate_limit(limit),
                )
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?;
            (
                filter_indexable_chunks(chunks, &self.index_settings),
                all_directories.clone(),
            )
        } else {
            let directory_paths = candidate_directories
                .iter()
                .map(|directory| directory.path.clone())
                .collect::<Vec<_>>();
            let chunks = self
                .database
                .file_chunk_matches_in_directory_trees_with_modified_range(
                    &directory_paths,
                    temporal_filter.modified_range(),
                )
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?;
            (
                filter_indexable_chunks(chunks, &self.index_settings),
                candidate_directories,
            )
        };
        let temporal_directory_paths = temporal_filter
            .has_range()
            .then(|| temporal_matching_directory_paths(&chunks));

        let mut scores = HashMap::<String, CandidateScore>::new();
        let mut ancestor_classification_cache =
            HashMap::<String, Vec<DirectoryClassification>>::new();
        let mut directory_classification_cache =
            HashMap::<String, Vec<DirectoryClassification>>::new();
        let mut target_cache = HashMap::<String, String>::new();
        for chunk in chunks {
            let mut score = cosine_similarity(&query_embedding, &chunk.embedding);
            if score <= 0.0 {
                continue;
            }

            let classifications = if let Some(classifications) =
                ancestor_classification_cache.get(&chunk.directory_path)
            {
                classifications.clone()
            } else {
                let classifications = self
                    .database
                    .ancestor_classifications(&chunk.directory_path)
                    .await
                    .map_err(|source| SearchError::LoadDirectories { source })?;
                ancestor_classification_cache
                    .insert(chunk.directory_path.clone(), classifications.clone());
                classifications
            };
            score += lexical_boost(
                &query_terms,
                [
                    chunk.content.as_str(),
                    chunk.file_name.as_str(),
                    chunk.directory_path.as_str(),
                ],
            );
            let path_match =
                path_match_boost(&path_terms, &chunk.directory_path, None, path_term_stats);
            score += path_match.score;
            let path_matched_terms = path_match.matched_terms.clone();
            let mut matched_terms = path_match.matched_terms;
            matched_terms.extend(matched_terms_in(
                &path_terms,
                [
                    chunk.content.as_str(),
                    chunk.file_name.as_str(),
                    chunk.directory_path.as_str(),
                ],
            ));
            score += classification_boost(&query_terms, &classifications);
            score *= temporal_filter.score_multiplier(
                chunk
                    .file_modified_unix_seconds
                    .max(chunk.directory_modified_unix_seconds),
            );
            if !chunk.is_current {
                score *= 0.55;
            }

            let target = navigation_target(
                self.database,
                &mut target_cache,
                &chunk.directory_path,
                &path_terms,
                &self.generic_terms,
            )
            .await?;
            score += direct_target_match_boost(&target, &chunk.directory_path, &path_matched_terms);

            update_candidate_score(
                &mut scores,
                target,
                score,
                matched_terms,
                path_matched_terms,
                &chunk.directory_path,
            );
        }

        for directory in directories {
            if let Some(matching_paths) = &temporal_directory_paths
                && !temporal_filter.matches(directory.modified_unix_seconds)
                && !directory_tree_has_temporal_match(&directory.path, matching_paths)
            {
                continue;
            }

            let classifications = if let Some(classifications) =
                directory_classification_cache.get(&directory.path)
            {
                classifications.clone()
            } else {
                let classifications = self
                    .database
                    .directory_classifications(&directory.path)
                    .await
                    .map_err(|source| SearchError::LoadDirectories { source })?;
                directory_classification_cache
                    .insert(directory.path.clone(), classifications.clone());
                classifications
            };
            let path_match = path_match_boost(
                &path_terms,
                &directory.path,
                Some(&directory.name),
                path_term_stats,
            );
            let score = classification_boost(&query_terms, &classifications)
                + lexical_boost(
                    &query_terms,
                    [directory.path.as_str(), directory.name.as_str()],
                )
                + path_match.score;
            if score <= 0.0 {
                continue;
            }
            let path_matched_terms = path_match.matched_terms.clone();
            let mut matched_terms = path_match.matched_terms;
            matched_terms.extend(matched_terms_in(
                &path_terms,
                [directory.path.as_str(), directory.name.as_str()],
            ));

            let target = navigation_target(
                self.database,
                &mut target_cache,
                &directory.path,
                &path_terms,
                &self.generic_terms,
            )
            .await?;
            let score =
                score + direct_target_match_boost(&target, &directory.path, &path_matched_terms);

            update_candidate_score(
                &mut scores,
                target,
                score,
                matched_terms,
                path_matched_terms,
                &directory.path,
            );
        }

        let mut results = scores
            .into_iter()
            .filter_map(|(path, score)| {
                let score = score.final_score(&path_terms, path_term_stats);
                (score > 0.0).then_some(SearchResult { path, score })
            })
            .collect::<Vec<_>>();

        results.sort_by(compare_results);
        results.truncate(limit);
        Ok(results)
    }

    pub async fn dry_run(&self, query: &str, limit: usize) -> Result<SearchDryRun> {
        self.dry_run_with_optional_cache(query, limit, None).await
    }

    pub async fn dry_run_with_cache(
        &self,
        query: &str,
        limit: usize,
        cache: &SearchCache,
    ) -> Result<SearchDryRun> {
        self.dry_run_with_cache_status(query, limit, cache, CacheDryRunStatus::Hit)
            .await
    }

    pub async fn dry_run_with_cache_status(
        &self,
        query: &str,
        limit: usize,
        cache: &SearchCache,
        cache_status: CacheDryRunStatus,
    ) -> Result<SearchDryRun> {
        self.dry_run_with_optional_cache(query, limit, Some((cache, cache_status)))
            .await
    }

    async fn dry_run_with_optional_cache(
        &self,
        query: &str,
        limit: usize,
        cache: Option<(&SearchCache, CacheDryRunStatus)>,
    ) -> Result<SearchDryRun> {
        let temporal_query = TemporalQuery::from_query(query);
        let search_query = temporal_query.search_query();
        let path_terms = path_query_terms(search_query);
        let loaded_cache;
        let (cache, cache_status) = if let Some((cache, cache_status)) = cache {
            (cache, cache_status)
        } else {
            loaded_cache =
                SearchCache::load(self.database, &self.generic_terms, &self.index_settings).await?;
            (&loaded_cache, CacheDryRunStatus::Miss)
        };
        let all_directories = &cache.directories;
        let candidate_terms = candidate_filter_terms(&path_terms, &self.generic_terms);
        let sql_candidate_documents = if candidate_terms.is_empty() {
            Vec::new()
        } else {
            self.database
                .directory_search_candidates_by_terms(&candidate_terms)
                .await
                .map_err(|source| SearchError::LoadDirectories { source })?
                .into_iter()
                .filter(|directory| {
                    is_indexable_directory_path(&directory.path, &self.index_settings)
                })
                .collect()
        };
        let sql_candidate_paths = sql_candidate_documents
            .iter()
            .map(|directory| directory.path.clone())
            .collect::<HashSet<_>>();
        let sql_candidate_directories = sql_candidate_documents
            .iter()
            .map(|directory| directory.path.clone())
            .collect::<Vec<_>>();

        let mut candidate_directories = sql_candidate_documents;
        add_fuzzy_directory_candidates(
            &mut candidate_directories,
            all_directories,
            &candidate_terms,
        );
        let fuzzy_candidate_directories = candidate_directories
            .iter()
            .filter(|directory| !sql_candidate_paths.contains(&directory.path))
            .map(|directory| directory.path.clone())
            .collect::<Vec<_>>();

        let query_embedding = self
            .embedder
            .embed_query(search_query)
            .map_err(|source| SearchError::EmbedQuery { source })?;
        let chunks = if candidate_directories.is_empty() {
            self.database
                .nearest_file_chunk_matches_with_modified_range(
                    &query_embedding,
                    temporal_query.filter.modified_range(),
                    semantic_candidate_limit(limit),
                )
                .await
                .map_err(|source| SearchError::LoadDirectories { source })
                .map(|chunks| filter_indexable_chunks(chunks, &self.index_settings))?
        } else {
            let directory_paths = candidate_directories
                .iter()
                .map(|directory| directory.path.clone())
                .collect::<Vec<_>>();
            self.database
                .file_chunk_matches_in_directory_trees_with_modified_range(
                    &directory_paths,
                    temporal_query.filter.modified_range(),
                )
                .await
                .map_err(|source| SearchError::LoadDirectories { source })
                .map(|chunks| filter_indexable_chunks(chunks, &self.index_settings))?
        };
        let mut embedding_scores = chunks
            .into_iter()
            .map(|chunk| EmbeddingScore {
                directory_path: chunk.directory_path,
                file_path: chunk.file_path,
                file_name: chunk.file_name,
                is_current: chunk.is_current,
                cosine_score: cosine_similarity(&query_embedding, &chunk.embedding),
                content_preview: content_preview(&chunk.content),
            })
            .collect::<Vec<_>>();
        embedding_scores.sort_by(|left, right| {
            right
                .cosine_score
                .partial_cmp(&left.cosine_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.directory_path.cmp(&right.directory_path))
                .then_with(|| left.file_path.cmp(&right.file_path))
        });

        let results = self
            .search_with_temporal_query(temporal_query.clone(), limit, Some(cache))
            .await?;

        Ok(SearchDryRun {
            query: query.to_string(),
            temporal: TemporalDryRun::from_query(&temporal_query),
            cache: CacheDryRun {
                status: cache_status,
                directory_count: all_directories.len(),
            },
            candidate_terms,
            sql_candidate_directories,
            fuzzy_candidate_directories,
            embedding_scores,
            results,
        })
    }
}

fn filter_indexable_directories(
    directories: Vec<IndexedDocument>,
    index_settings: &IndexSettings,
) -> Vec<IndexedDocument> {
    directories
        .into_iter()
        .filter(|directory| is_indexable_directory_path(&directory.path, index_settings))
        .collect()
}

fn filter_indexable_chunks(
    chunks: Vec<FileChunkMatch>,
    index_settings: &IndexSettings,
) -> Vec<FileChunkMatch> {
    chunks
        .into_iter()
        .filter(|chunk| is_indexable_directory_path(&chunk.directory_path, index_settings))
        .collect()
}

fn semantic_candidate_limit(result_limit: usize) -> usize {
    if result_limit == 0 {
        return 1_000;
    }

    result_limit.saturating_mul(200).clamp(1_000, 10_000)
}

fn is_indexable_directory_path(path: &str, index_settings: &IndexSettings) -> bool {
    path.split('/')
        .filter(|component| !component.is_empty())
        .all(|component| !index_settings.is_excluded_directory_component(component))
}

fn high_confidence_directory_candidate<'a>(
    directories: &'a [IndexedDocument],
    candidate_terms: &[String],
) -> Option<&'a IndexedDocument> {
    if directories.len() != 1 || candidate_terms.is_empty() {
        return None;
    }

    let directory = &directories[0];
    path_candidate_matches_all_terms(candidate_terms, &directory.path, &directory.name)
        .then_some(directory)
}

fn add_fuzzy_directory_candidates(
    candidates: &mut Vec<IndexedDocument>,
    all_directories: &[IndexedDocument],
    candidate_terms: &[String],
) {
    if candidate_terms.is_empty() {
        return;
    }

    let mut seen_paths = candidates
        .iter()
        .map(|directory| directory.path.clone())
        .collect::<HashSet<_>>();
    for directory in all_directories {
        if seen_paths.contains(&directory.path) {
            continue;
        }
        if path_candidate_matches_any_term(candidate_terms, &directory.path, &directory.name) {
            seen_paths.insert(directory.path.clone());
            candidates.push(directory.clone());
        }
    }
}

fn temporal_matching_directory_paths(chunks: &[FileChunkMatch]) -> HashSet<String> {
    chunks
        .iter()
        .map(|chunk| chunk.directory_path.clone())
        .collect()
}

fn directory_tree_has_temporal_match(path: &str, matching_paths: &HashSet<String>) -> bool {
    let child_prefix = format!("{path}/");
    matching_paths
        .iter()
        .any(|matching_path| matching_path == path || matching_path.starts_with(&child_prefix))
}

#[derive(Debug, Default)]
struct CandidateScore {
    score: f32,
    matched_terms: HashSet<String>,
    path_matched_terms: HashSet<String>,
    direct_path_matched_terms: HashSet<String>,
    nested_path_matched_terms: HashSet<String>,
}

impl CandidateScore {
    fn update(
        &mut self,
        score: f32,
        matched_terms: HashSet<String>,
        path_matched_terms: HashSet<String>,
        direct_path_match: bool,
    ) {
        self.score = self.score.max(score);
        self.matched_terms.extend(matched_terms);
        if direct_path_match {
            self.direct_path_matched_terms
                .extend(path_matched_terms.iter().cloned());
        } else {
            self.nested_path_matched_terms
                .extend(path_matched_terms.iter().cloned());
        }
        self.path_matched_terms.extend(path_matched_terms);
    }

    fn final_score(&self, query_terms: &[String], stats: &PathTermStats) -> f32 {
        if query_terms.is_empty() {
            return self.score;
        }

        let total_weight = query_terms
            .iter()
            .map(|term| stats.term_weight(term))
            .sum::<f32>();
        if total_weight <= 0.0 {
            return self.score;
        }

        let matched_weight = query_terms
            .iter()
            .filter(|term| self.matched_terms.contains(term.as_str()))
            .map(|term| stats.term_weight(term))
            .sum::<f32>();
        let coverage = matched_weight / total_weight;
        let path_matched_weight = query_terms
            .iter()
            .filter(|term| self.path_matched_terms.contains(term.as_str()))
            .map(|term| stats.term_weight(term))
            .sum::<f32>();
        let path_coverage = path_matched_weight / total_weight;
        let direct_path_matched_weight = query_terms
            .iter()
            .filter(|term| self.direct_path_matched_terms.contains(term.as_str()))
            .map(|term| stats.term_weight(term))
            .sum::<f32>();
        let nested_only_path_matched_weight = query_terms
            .iter()
            .filter(|term| {
                self.nested_path_matched_terms.contains(term.as_str())
                    && !self.direct_path_matched_terms.contains(term.as_str())
            })
            .map(|term| stats.term_weight(term))
            .sum::<f32>();
        let direct_path_coverage = direct_path_matched_weight / total_weight;
        let nested_only_path_coverage = nested_only_path_matched_weight / total_weight;
        let full_coverage_bonus = if self.matched_terms.len() >= query_terms.len() {
            1.25
        } else {
            0.0
        };
        let content_only_penalty = if path_matched_weight <= 0.0 && matched_weight > 0.0 {
            0.75
        } else {
            0.0
        };

        self.score
            + coverage * 1.4
            + path_coverage * 4.0
            + full_coverage_bonus
            + direct_path_coverage * 4.0
            - nested_only_path_coverage * 3.0
            - content_only_penalty
    }
}

fn update_candidate_score(
    scores: &mut HashMap<String, CandidateScore>,
    target: String,
    score: f32,
    matched_terms: HashSet<String>,
    path_matched_terms: HashSet<String>,
    evidence_path: &str,
) {
    let direct_path_match = target == evidence_path;
    scores.entry(target).or_default().update(
        score,
        matched_terms,
        path_matched_terms,
        direct_path_match,
    );
}

fn direct_target_match_boost(
    target_path: &str,
    evidence_path: &str,
    path_matched_terms: &HashSet<String>,
) -> f32 {
    if path_matched_terms.is_empty() {
        0.0
    } else if target_path == evidence_path {
        1.75
    } else {
        -2.0
    }
}

async fn navigation_target(
    database: &Database,
    target_cache: &mut HashMap<String, String>,
    path: &str,
    path_terms: &[String],
    generic_terms: &HashSet<String>,
) -> Result<String> {
    if should_navigate_to_path(path_terms, path, generic_terms) {
        return Ok(path.to_string());
    }

    if let Some(target) = target_cache.get(path) {
        return Ok(target.clone());
    }

    let target = database
        .general_indexed_directory(path)
        .await
        .map_err(|source| SearchError::LoadDirectories { source })?;
    target_cache.insert(path.to_string(), target.clone());
    Ok(target)
}

fn should_navigate_to_path(
    path_terms: &[String],
    path: &str,
    generic_terms: &HashSet<String>,
) -> bool {
    let Some(name) = PathName::from_path(path) else {
        return false;
    };
    let name_tokens = path_tokens(name);
    let query_terms = path_terms
        .iter()
        .filter(|term| !is_generic_nav_term(term, generic_terms))
        .cloned()
        .collect::<Vec<_>>();
    let distinct_query_terms = query_terms.iter().collect::<HashSet<_>>();
    if distinct_query_terms.len() < 2 {
        return false;
    }

    let leaf_matches = query_terms.iter().any(|term| {
        name_tokens.iter().any(|token| {
            token == term
                || is_fuzzy_path_token_match(term, token)
                || is_partial_path_token_match(term, token)
        })
    });

    leaf_matches && path_candidate_matches_all_terms(&query_terms, path, name)
}

fn path_candidate_matches_any_term(query_terms: &[String], path: &str, name: &str) -> bool {
    let mut candidate_tokens = path_tokens(path);
    candidate_tokens.extend(path_tokens(name));
    query_terms.iter().any(|term| {
        candidate_tokens.iter().any(|token| {
            token == term
                || is_fuzzy_path_token_match(term, token)
                || is_partial_path_token_match(term, token)
        })
    })
}

fn path_candidate_matches_all_terms(query_terms: &[String], path: &str, name: &str) -> bool {
    let mut candidate_tokens = path_tokens(path);
    candidate_tokens.extend(path_tokens(name));
    query_terms.iter().all(|term| {
        candidate_tokens.iter().any(|token| {
            token == term
                || is_fuzzy_path_token_match(term, token)
                || is_partial_path_token_match(term, token)
        })
    })
}

#[derive(Debug, Clone)]
struct PathTermStats {
    directory_count: usize,
    directory_name_frequency: HashMap<String, usize>,
    generic_terms: HashSet<String>,
}

impl PathTermStats {
    fn from_directories(directories: &[IndexedDocument], generic_terms: &HashSet<String>) -> Self {
        let mut directory_name_frequency = HashMap::new();
        for directory in directories {
            let unique_terms = path_tokens(&directory.name)
                .into_iter()
                .collect::<HashSet<_>>();
            for term in unique_terms {
                *directory_name_frequency.entry(term).or_insert(0) += 1;
            }
        }

        Self {
            directory_count: directories.len(),
            directory_name_frequency,
            generic_terms: generic_terms.clone(),
        }
    }

    fn term_weight(&self, term: &str) -> f32 {
        let document_count = self.directory_count.max(1) as f32;
        let frequency = self
            .directory_name_frequency
            .get(term)
            .copied()
            .unwrap_or(0) as f32;
        let rarity = ((document_count + 1.0) / (frequency + 1.0))
            .ln()
            .clamp(0.0, 2.0);
        let generic_multiplier = if is_generic_nav_term(term, &self.generic_terms) {
            0.45
        } else {
            1.0
        };
        let length_bonus = if term.len() >= 5 { 0.2 } else { 0.0 };

        (1.0 + rarity + length_bonus) * generic_multiplier
    }
}

fn is_generic_nav_term(term: &str, generic_terms: &HashSet<String>) -> bool {
    generic_terms.contains(term)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TemporalQuery {
    original_query: String,
    cleaned_query: String,
    matched_phrase: Option<String>,
    filter: TemporalFilter,
}

impl TemporalQuery {
    fn from_query(query: &str) -> Self {
        Self::from_query_at(query, Local::now())
    }

    fn from_query_at<Tz>(query: &str, now: DateTime<Tz>) -> Self
    where
        Tz: TimeZone,
    {
        let lower = query.to_ascii_lowercase();
        let current_date = now.date_naive();
        let timezone = now.timezone();

        if lower.contains("last month") {
            let (year, month) = previous_month(current_date.year(), current_date.month());
            let start_date = first_day_of_month(year, month);
            let end_date = first_day_of_month(current_date.year(), current_date.month());
            return Self::with_range(query, "last month", start_date, end_date, &timezone);
        }

        if lower.contains("this month") {
            let start_date = first_day_of_month(current_date.year(), current_date.month());
            let (next_year, next_month) = next_month(current_date.year(), current_date.month());
            let end_date = first_day_of_month(next_year, next_month);
            return Self::with_range(query, "this month", start_date, end_date, &timezone);
        }

        if lower.contains("last week") {
            let current_week_start = current_date
                - Duration::days(i64::from(current_date.weekday().num_days_from_monday()));
            let start_date = current_week_start - Duration::days(7);
            return Self::with_range(
                query,
                "last week",
                start_date,
                current_week_start,
                &timezone,
            );
        }

        if lower.contains("yesterday") {
            let start_date = current_date - Duration::days(1);
            return Self::with_range(query, "yesterday", start_date, current_date, &timezone);
        }

        if lower.contains("today") {
            let end_date = current_date + Duration::days(1);
            return Self::with_range(query, "today", current_date, end_date, &timezone);
        }

        if lower.contains("recently") {
            return Self::with_open_range(query, "recently", now.timestamp() - 30 * 24 * 60 * 60);
        }

        if lower.contains("recent") {
            return Self::with_open_range(query, "recent", now.timestamp() - 30 * 24 * 60 * 60);
        }

        Self {
            original_query: query.to_string(),
            cleaned_query: normalize_query_whitespace(query),
            matched_phrase: None,
            filter: TemporalFilter::default(),
        }
    }

    fn with_range<Tz>(
        query: &str,
        phrase: &str,
        start_date: NaiveDate,
        end_date: NaiveDate,
        timezone: &Tz,
    ) -> Self
    where
        Tz: TimeZone,
    {
        Self {
            original_query: query.to_string(),
            cleaned_query: remove_temporal_phrase(query, phrase),
            matched_phrase: Some(phrase.to_string()),
            filter: TemporalFilter {
                start_unix_seconds: Some(local_midnight_unix_seconds(start_date, timezone)),
                end_unix_seconds: Some(local_midnight_unix_seconds(end_date, timezone)),
            },
        }
    }

    fn with_open_range(query: &str, phrase: &str, start_unix_seconds: i64) -> Self {
        Self {
            original_query: query.to_string(),
            cleaned_query: remove_temporal_phrase(query, phrase),
            matched_phrase: Some(phrase.to_string()),
            filter: TemporalFilter {
                start_unix_seconds: Some(start_unix_seconds),
                end_unix_seconds: None,
            },
        }
    }

    fn search_query(&self) -> &str {
        if self.cleaned_query.trim().is_empty() {
            &self.original_query
        } else {
            &self.cleaned_query
        }
    }
}

impl TemporalDryRun {
    fn from_query(query: &TemporalQuery) -> Self {
        Self {
            cleaned_query: query.cleaned_query.clone(),
            semantic_query: query.search_query().to_string(),
            matched_phrase: query.matched_phrase.clone(),
            start_unix_seconds: query.filter.start_unix_seconds,
            end_unix_seconds: query.filter.end_unix_seconds,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct TemporalFilter {
    start_unix_seconds: Option<i64>,
    end_unix_seconds: Option<i64>,
}

impl TemporalFilter {
    fn has_range(self) -> bool {
        self.start_unix_seconds.is_some() || self.end_unix_seconds.is_some()
    }

    fn modified_range(self) -> Option<ModifiedTimeRange> {
        self.has_range().then_some(ModifiedTimeRange {
            start_unix_seconds: self.start_unix_seconds,
            end_unix_seconds: self.end_unix_seconds,
        })
    }

    fn matches(self, modified_unix_seconds: i64) -> bool {
        self.modified_range()
            .is_none_or(|range| range.contains(modified_unix_seconds))
    }

    fn score_multiplier(self, modified_unix_seconds: i64) -> f32 {
        if !self.has_range() {
            return 1.0;
        }

        if self.matches(modified_unix_seconds) {
            1.35
        } else {
            0.25
        }
    }
}

fn remove_temporal_phrase(query: &str, phrase: &str) -> String {
    let lower = query.to_ascii_lowercase();
    let Some(start) = lower.find(phrase) else {
        return normalize_query_whitespace(query);
    };
    let end = start + phrase.len();
    normalize_query_whitespace(&format!("{} {}", &query[..start], &query[end..]))
}

fn normalize_query_whitespace(query: &str) -> String {
    query.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn previous_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

fn first_day_of_month(year: i32, month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, 1).expect("valid first day of month")
}

fn local_midnight_unix_seconds<Tz>(date: NaiveDate, timezone: &Tz) -> i64
where
    Tz: TimeZone,
{
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .expect("midnight should be a valid naive time");
    match timezone.from_local_datetime(&midnight) {
        LocalResult::Single(time) => time.timestamp(),
        LocalResult::Ambiguous(first, second) => first.timestamp().min(second.timestamp()),
        LocalResult::None => timezone.from_utc_datetime(&midnight).timestamp(),
    }
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.len() > 2)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn path_query_terms(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.len() > 1)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn candidate_filter_terms(path_terms: &[String], generic_terms: &HashSet<String>) -> Vec<String> {
    path_terms
        .iter()
        .filter(|term| {
            !is_generic_nav_term(term, generic_terms) && (term.len() > 2 || term.as_str() == "cd")
        })
        .cloned()
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

fn matched_terms_in<'a>(
    query_terms: &[String],
    haystacks: impl IntoIterator<Item = &'a str>,
) -> HashSet<String> {
    let haystack = haystacks
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("\n");

    query_terms
        .iter()
        .filter(|term| haystack.contains(term.as_str()))
        .cloned()
        .collect()
}

#[derive(Debug, Default)]
struct PathMatch {
    score: f32,
    matched_terms: HashSet<String>,
}

fn path_match_boost(
    query_terms: &[String],
    path: &str,
    name: Option<&str>,
    stats: &PathTermStats,
) -> PathMatch {
    if query_terms.is_empty() {
        return PathMatch::default();
    }

    let candidate_path_tokens = path_tokens(path);
    let name_tokens = name.map(path_tokens).unwrap_or_else(|| {
        PathName::from_path(path)
            .map(path_tokens)
            .unwrap_or_default()
    });

    let path_exact_matches = query_terms
        .iter()
        .filter(|term| candidate_path_tokens.contains(term))
        .cloned()
        .collect::<HashSet<_>>();
    let name_exact_matches = query_terms
        .iter()
        .filter(|term| name_tokens.contains(term))
        .cloned()
        .collect::<HashSet<_>>();
    let path_fuzzy_matches = fuzzy_matched_terms(query_terms, &candidate_path_tokens);
    let name_fuzzy_matches = fuzzy_matched_terms(query_terms, &name_tokens);
    let path_partial_matches = partial_matched_terms(query_terms, &candidate_path_tokens);
    let name_partial_matches = partial_matched_terms(query_terms, &name_tokens);
    let path_lower = path.to_ascii_lowercase();
    let substring_matches = query_terms
        .iter()
        .filter(|term| path_lower.contains(term.as_str()))
        .cloned()
        .collect::<HashSet<_>>();
    let mut matched_terms = HashSet::new();
    matched_terms.extend(path_exact_matches.iter().cloned());
    matched_terms.extend(name_exact_matches.iter().cloned());
    matched_terms.extend(substring_matches.iter().cloned());
    matched_terms.extend(path_fuzzy_matches.iter().cloned());
    matched_terms.extend(name_fuzzy_matches.iter().cloned());
    matched_terms.extend(path_partial_matches.iter().cloned());
    matched_terms.extend(name_partial_matches.iter().cloned());

    let path_exact_score = path_exact_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.45)
        .sum::<f32>();
    let name_exact_score = name_exact_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.65)
        .sum::<f32>();
    let substring_score = substring_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.08)
        .sum::<f32>();
    let path_fuzzy_score = path_fuzzy_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.32)
        .sum::<f32>();
    let name_fuzzy_score = name_fuzzy_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.48)
        .sum::<f32>();
    let path_partial_score = path_partial_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.24)
        .sum::<f32>();
    let name_partial_score = name_partial_matches
        .iter()
        .map(|term| stats.term_weight(term) * 0.36)
        .sum::<f32>();
    let mut score = path_exact_score
        + name_exact_score
        + substring_score
        + path_fuzzy_score
        + name_fuzzy_score
        + path_partial_score
        + name_partial_score;

    if path_exact_matches.len() == query_terms.len() {
        score += 1.5;
    }
    if name_exact_matches.len() == query_terms.len() {
        score += 2.5;
    }
    if matched_terms.len() == query_terms.len() && !path_fuzzy_matches.is_empty() {
        score += 1.0;
    }
    if matched_terms.len() == query_terms.len() && !name_fuzzy_matches.is_empty() {
        score += 1.5;
    }
    if matched_terms.len() == query_terms.len() && !path_partial_matches.is_empty() {
        score += 0.75;
    }
    if matched_terms.len() == query_terms.len() && !name_partial_matches.is_empty() {
        score += 1.0;
    }

    PathMatch {
        score,
        matched_terms,
    }
}

fn fuzzy_matched_terms(query_terms: &[String], candidate_tokens: &[String]) -> HashSet<String> {
    query_terms
        .iter()
        .filter(|term| {
            candidate_tokens
                .iter()
                .any(|candidate| is_fuzzy_path_token_match(term, candidate))
        })
        .cloned()
        .collect()
}

fn partial_matched_terms(query_terms: &[String], candidate_tokens: &[String]) -> HashSet<String> {
    query_terms
        .iter()
        .filter(|term| {
            candidate_tokens
                .iter()
                .any(|candidate| is_partial_path_token_match(term, candidate))
        })
        .cloned()
        .collect()
}

fn is_fuzzy_path_token_match(query_term: &str, candidate_token: &str) -> bool {
    if query_term == candidate_token || query_term.len() < 4 || candidate_token.len() < 4 {
        return false;
    }

    let length_delta = query_term.len().abs_diff(candidate_token.len());
    if length_delta > 2 {
        return false;
    }

    let distance = damerau_levenshtein_distance(query_term, candidate_token);
    if distance <= 1 {
        return true;
    }

    query_term.len().min(candidate_token.len()) >= 5
        && query_term.len().max(candidate_token.len()) >= 6
        && distance == 2
        && has_common_bigram(query_term, candidate_token)
}

fn is_partial_path_token_match(query_term: &str, candidate_token: &str) -> bool {
    if query_term == candidate_token || query_term.len() < 4 || candidate_token.len() < 3 {
        return false;
    }
    if is_fuzzy_path_token_match(query_term, candidate_token) {
        return false;
    }

    common_prefix_len(query_term, candidate_token) >= 3
}

fn common_prefix_len(left: &str, right: &str) -> usize {
    left.chars()
        .zip(right.chars())
        .take_while(|(left, right)| left == right)
        .count()
}

fn has_common_bigram(left: &str, right: &str) -> bool {
    let right_bigrams = right.as_bytes().windows(2).collect::<HashSet<_>>();

    left.as_bytes()
        .windows(2)
        .any(|bigram| right_bigrams.contains(bigram))
}

fn damerau_levenshtein_distance(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut previous_previous = Vec::<usize>::new();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();

    for (left_index, left_char) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(left_index + 1);

        for (right_index, right_char) in right.iter().enumerate() {
            let deletion = previous[right_index + 1] + 1;
            let insertion = current[right_index] + 1;
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            let mut distance = deletion.min(insertion).min(substitution);

            if left_index > 0
                && right_index > 0
                && left[left_index] == right[right_index - 1]
                && left[left_index - 1] == right[right_index]
            {
                distance = distance.min(previous_previous[right_index - 1] + 1);
            }

            current.push(distance);
        }

        previous_previous = previous;
        previous = current;
    }

    previous[right.len()]
}

struct PathName;

impl PathName {
    fn from_path(path: &str) -> Option<&str> {
        path.rsplit(['/', '\\']).find(|part| !part.is_empty())
    }
}

fn path_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| term.len() > 1)
        .map(|term| term.to_ascii_lowercase())
        .collect()
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

fn content_preview(content: &str) -> String {
    let preview = content.split_whitespace().collect::<Vec<_>>().join(" ");
    preview.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::config::Settings;
    use crate::db::{Database, DocumentKind, IndexedFile, IndexedFileChunk};
    use crate::embed::{EmbedError, FakeEmbedder};
    use crate::index::Indexer;

    struct PanicEmbedder;

    impl Embedder for PanicEmbedder {
        fn dimensions(&self) -> usize {
            32
        }

        fn embed(&self, _text: &str) -> crate::embed::Result<Vec<f32>> {
            panic!("high-confidence directory match should not embed the query")
        }

        fn embed_query(&self, _text: &str) -> crate::embed::Result<Vec<f32>> {
            Err(EmbedError::Model {
                message: "high-confidence directory match should not embed the query".to_string(),
            })
        }
    }

    fn fixed_time(
        year: i32,
        month: u32,
        day: u32,
        hour: u32,
    ) -> chrono::DateTime<chrono::FixedOffset> {
        use chrono::TimeZone as _;

        chrono::FixedOffset::east_opt(0)
            .unwrap()
            .with_ymd_and_hms(year, month, day, hour, 0, 0)
            .single()
            .unwrap()
    }

    async fn insert_directory_with_chunk(
        database: &Database,
        embedder: &FakeEmbedder,
        directory_path: &str,
        file_modified_unix_seconds: i64,
        content: &str,
    ) {
        let directory_name = directory_path
            .rsplit('/')
            .next()
            .unwrap_or(directory_path)
            .to_string();
        database
            .upsert_document(&IndexedDocument {
                path: directory_path.to_string(),
                name: directory_name,
                kind: DocumentKind::Directory,
                parent_path: Some("/tmp".to_string()),
                searchable_text: directory_path.to_string(),
                embedding: embedder.embed(directory_path).unwrap(),
                metadata_fingerprint: format!("directory:{directory_path}"),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 1,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: file_modified_unix_seconds,
            })
            .await
            .unwrap();

        let file = IndexedFile {
            path: format!("{directory_path}/README.md"),
            directory_path: directory_path.to_string(),
            name: "README.md".to_string(),
            extension: Some("md".to_string()),
            size_bytes: u64::try_from(content.len()).unwrap(),
            created_unix_seconds: None,
            modified_unix_seconds: file_modified_unix_seconds,
            accessed_unix_seconds: None,
            readonly: false,
            content_fingerprint: format!("mtime:{file_modified_unix_seconds}:{content}"),
            indexed_unix_seconds: file_modified_unix_seconds,
        };
        let chunks = vec![IndexedFileChunk {
            file_path: file.path.clone(),
            directory_path: file.directory_path.clone(),
            chunk_index: 0,
            content: content.to_string(),
            embedding: embedder.embed(content).unwrap(),
            start_byte: 0,
            end_byte: u64::try_from(content.len()).unwrap(),
            indexed_unix_seconds: file_modified_unix_seconds,
        }];
        database
            .upsert_files_with_chunks(&[(&file, chunks.as_slice())])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn returns_best_directory_match() {
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
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("chrome extension", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, chrome.to_string_lossy());
    }

    #[tokio::test]
    async fn search_ignores_stale_builtin_noise_directories() {
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        insert_directory_with_chunk(
            &database,
            &embedder,
            "/tmp/Applications/Chrome Apps.localized",
            10,
            "Chrome extension application launcher",
        )
        .await;
        insert_directory_with_chunk(
            &database,
            &embedder,
            "/tmp/Projects/chrome-extension",
            10,
            "Chrome extension manifest browser popup",
        )
        .await;

        let results = Searcher::new(&database, &embedder)
            .search("chrome extension", 3)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "/tmp/Projects/chrome-extension");
    }

    #[test]
    fn temporal_parser_converts_last_month_to_calendar_range() {
        let parsed =
            TemporalQuery::from_query_at("that thing I did last month", fixed_time(2026, 6, 4, 12));
        let dry_run = TemporalDryRun::from_query(&parsed);

        assert_eq!(parsed.cleaned_query, "that thing I did");
        assert_eq!(dry_run.matched_phrase.as_deref(), Some("last month"));
        assert_eq!(dry_run.semantic_query, "that thing I did");
        assert_eq!(
            parsed.filter.modified_range(),
            Some(ModifiedTimeRange {
                start_unix_seconds: Some(fixed_time(2026, 5, 1, 0).timestamp()),
                end_unix_seconds: Some(fixed_time(2026, 6, 1, 0).timestamp()),
            })
        );
    }

    #[test]
    fn temporal_parser_converts_yesterday_to_day_range() {
        let parsed =
            TemporalQuery::from_query_at("project from yesterday", fixed_time(2026, 6, 4, 12));

        assert_eq!(parsed.cleaned_query, "project from");
        assert_eq!(
            parsed.filter.modified_range(),
            Some(ModifiedTimeRange {
                start_unix_seconds: Some(fixed_time(2026, 6, 3, 0).timestamp()),
                end_unix_seconds: Some(fixed_time(2026, 6, 4, 0).timestamp()),
            })
        );
    }

    #[tokio::test]
    async fn temporal_query_filters_chunks_by_modified_time_before_ranking() {
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        insert_directory_with_chunk(
            &database,
            &embedder,
            "/tmp/may-project",
            fixed_time(2026, 5, 15, 12).timestamp(),
            "project notes",
        )
        .await;
        insert_directory_with_chunk(
            &database,
            &embedder,
            "/tmp/april-project",
            fixed_time(2026, 4, 15, 12).timestamp(),
            "project notes",
        )
        .await;

        let temporal_query =
            TemporalQuery::from_query_at("project last month", fixed_time(2026, 6, 4, 12));
        let results = Searcher::new(&database, &embedder)
            .search_with_temporal_query(temporal_query, 5, None)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, "/tmp/may-project");
        assert!(
            !results
                .iter()
                .any(|result| result.path == "/tmp/april-project")
        );
    }

    #[tokio::test]
    async fn returns_directory_by_deterministic_type_classification() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let rust = root.join("cds-rust");
        let notes = root.join("notes");
        fs::create_dir_all(&rust).unwrap();
        fs::create_dir_all(&notes).unwrap();
        fs::write(rust.join("Cargo.toml"), "[package]\nname = \"cds\"").unwrap();
        fs::write(notes.join("README.md"), "rust colored notes without cargo").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("rust project", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, rust.to_string_lossy());
    }

    #[tokio::test]
    async fn prefers_deeper_directory_when_query_has_strong_signal() {
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
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("migrations chrome extension", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, migrations.to_string_lossy());
    }

    #[tokio::test]
    async fn prefers_deeper_directory_when_leaf_name_matches_query() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let app = root.join("chrome-extension");
        let widgets = app.join("src/widgets");
        fs::create_dir_all(&widgets).unwrap();
        fs::write(
            widgets.join("README.md"),
            "toolbar widgets for chrome extension popup",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("widgets chrome extension", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, widgets.to_string_lossy());
    }

    #[tokio::test]
    async fn collapses_single_leaf_term_to_general_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("opencode");
        let nested = expected.join("github");
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("README.md"),
            "github workflow configuration and repository automation",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("github thing", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, expected.to_string_lossy());
    }

    #[tokio::test]
    async fn exact_path_tokens_beat_weaker_semantic_matches() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("cd-with-llm-search");
        let misleading = root.join("gollm");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(expected.join("README.md"), "semantic cd directory search").unwrap();
        fs::write(
            misleading.join("README.md"),
            "language model workspace with unrelated content",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("cd with llm", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, expected.to_string_lossy());
    }

    #[tokio::test]
    async fn fuzzy_path_tokens_tolerate_simple_misspellings() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("cd-with-llm-search");
        let misleading = root.join("opencode-llm");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(expected.join("README.md"), "semantic cd directory search").unwrap();
        fs::write(
            misleading.join("README.md"),
            "llm srach assistant workspace with exact typo terms",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("llm srach", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, expected.to_string_lossy());
    }

    #[tokio::test]
    async fn partial_path_tokens_match_query_dependent_prefixes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("gitplub");
        let misleading = root.join("opencode").join("github");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(expected.join("README.md"), "small project launcher").unwrap();
        fs::write(
            misleading.join("README.md"),
            "github clone repository github clone command documentation",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("github clone", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, expected.to_string_lossy());
    }

    #[tokio::test]
    async fn dry_run_reports_candidates_embedding_scores_and_winner() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("gitplub");
        let misleading = root.join("opencode").join("github");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(expected.join("README.md"), "small project launcher").unwrap();
        fs::write(
            misleading.join("README.md"),
            "github clone repository github clone command documentation",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let report = Searcher::new(&database, &embedder)
            .dry_run("github clone", 3)
            .await
            .unwrap();

        assert_eq!(report.cache.status, CacheDryRunStatus::Miss);
        assert!(report.cache.directory_count > 0);
        assert_eq!(report.temporal.matched_phrase, None);
        assert_eq!(report.temporal.semantic_query, "github clone");
        assert_eq!(report.candidate_terms, vec!["github", "clone"]);
        assert!(
            report
                .sql_candidate_directories
                .iter()
                .any(|path| path == &misleading.to_string_lossy())
        );
        assert!(
            report
                .fuzzy_candidate_directories
                .iter()
                .any(|path| path == &expected.to_string_lossy())
        );
        assert!(!report.embedding_scores.is_empty());
        assert_eq!(
            report.results.first().unwrap().path,
            expected.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn distinctive_path_term_beats_generic_category_term() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("racoon-dash");
        let misleading = root.join("fuckin-game");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&misleading).unwrap();
        fs::write(expected.join("README.md"), "dash runner score levels").unwrap();
        fs::write(
            misleading.join("README.md"),
            "arcade game engine sprites levels controller",
        )
        .unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("racoon game", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, expected.to_string_lossy());
    }

    #[tokio::test]
    async fn single_high_confidence_directory_match_skips_semantic_search() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let expected = root.join("racoon-dash");
        let other = root.join("arcade-game");
        fs::create_dir_all(&expected).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(expected.join("README.md"), "dash runner score levels").unwrap();
        fs::write(other.join("README.md"), "arcade game engine levels").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let index_embedder = FakeEmbedder::default();
        Indexer::new(&settings, &database, &index_embedder)
            .index_roots(vec![root])
            .await
            .unwrap();

        let results = Searcher::new(&database, &PanicEmbedder)
            .search("racoon dash", 3)
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, expected.to_string_lossy());
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn fuzzy_path_token_match_handles_typo_and_transposition() {
        assert!(is_fuzzy_path_token_match("srach", "search"));
        assert!(is_fuzzy_path_token_match("serach", "search"));
        assert!(is_fuzzy_path_token_match("gihub", "github"));
        assert!(!is_fuzzy_path_token_match("app", "api"));
        assert!(!is_fuzzy_path_token_match("clone", "code"));
        assert!(!is_fuzzy_path_token_match("code", "search"));
    }

    #[test]
    fn partial_path_token_match_handles_shared_prefixes() {
        assert!(
            is_fuzzy_path_token_match("github", "gitplub")
                || is_partial_path_token_match("github", "gitplub")
        );
        assert!(is_partial_path_token_match("photography", "photo"));
        assert!(is_partial_path_token_match("typescript", "typefully"));
        assert!(!is_partial_path_token_match("clone", "gitplub"));
        assert!(!is_partial_path_token_match("code", "gitplub"));
        assert!(!is_partial_path_token_match("app", "apple"));
    }

    #[test]
    fn candidate_filter_terms_uses_configured_generic_terms() {
        let terms = vec!["racoon".to_string(), "game".to_string()];
        let generic_terms = HashSet::from(["game".to_string()]);

        assert_eq!(
            candidate_filter_terms(&terms, &generic_terms),
            vec!["racoon".to_string()]
        );
        assert_eq!(candidate_filter_terms(&terms, &HashSet::new()), terms);
    }

    #[tokio::test]
    async fn searches_historical_file_chunk_versions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("Projects");
        let app = root.join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("README.md"), "legacy photon dashboard").unwrap();

        let settings = Settings::default();
        let database = Database::open_in_memory().await.unwrap();
        let embedder = FakeEmbedder::default();
        let indexer = Indexer::new(&settings, &database, &embedder);
        indexer.index_roots(vec![root.clone()]).await.unwrap();

        fs::write(app.join("README.md"), "modern analytics workspace").unwrap();
        indexer.index_roots(vec![root]).await.unwrap();

        let results = Searcher::new(&database, &embedder)
            .search("legacy photon", 3)
            .await
            .unwrap();

        assert_eq!(results.first().unwrap().path, app.to_string_lossy());
    }
}
