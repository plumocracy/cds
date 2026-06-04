use std::io;
use std::path::PathBuf;

use thiserror::Error;

use crate::config::ConfigError;
use crate::db::DbError;
use crate::embed::EmbedError;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("failed to expand configured index roots")]
    ExpandConfiguredRoots {
        #[source]
        source: ConfigError,
    },

    #[error("failed to index root {root}")]
    ScanRoot {
        root: PathBuf,
        #[source]
        source: Box<IndexError>,
    },

    #[error("failed to summarize {path}")]
    SummarizeDirectory {
        path: PathBuf,
        #[source]
        source: Box<IndexError>,
    },

    #[error("failed to store indexed document {path}")]
    StoreDocument {
        path: String,
        #[source]
        source: Box<DbError>,
    },

    #[error("failed to store indexed file {path}")]
    StoreFile {
        path: String,
        #[source]
        source: Box<DbError>,
    },

    #[error("failed to store indexed chunks for {path}")]
    StoreFileChunks {
        path: String,
        #[source]
        source: Box<DbError>,
    },

    #[error("failed to store directory classifications for {path}")]
    StoreDirectoryClassifications {
        path: String,
        #[source]
        source: Box<DbError>,
    },

    #[error("failed to prune excluded indexed path {path}")]
    PruneExcludedPath {
        path: String,
        #[source]
        source: Box<DbError>,
    },

    #[error("failed to embed summary for {path}")]
    EmbedSummary {
        path: PathBuf,
        #[source]
        source: EmbedError,
    },

    #[error("failed to read metadata for {path}")]
    ReadMetadata {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to read directory {path}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to read entry in {path}")]
    ReadDirectoryEntry {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to read file type for {path}")]
    ReadFileType {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to stat {path}")]
    StatFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to read {path}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse directory type definition {path}")]
    ParseDirectoryTypeDefinition {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}
