use std::io;
use std::num::TryFromIntError;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("failed to create database directory {path}")]
    CreateDatabaseDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to open database {path}")]
    OpenDatabase {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },

    #[error("database does not exist at {path}; run `cds --init` first")]
    MissingDatabase { path: PathBuf },

    #[error("failed to open in-memory database")]
    OpenInMemory {
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to migrate database")]
    Migrate {
        #[source]
        source: sqlx::migrate::MigrateError,
    },

    #[error("failed to prepare legacy database for SQLx migrations")]
    PrepareLegacyMigration {
        #[source]
        source: sqlx::Error,
    },

    #[error("embedding dimension overflows i64")]
    EmbeddingDimensionOverflow {
        #[source]
        source: TryFromIntError,
    },

    #[error("metadata size overflows i64")]
    MetadataSizeOverflow {
        #[source]
        source: TryFromIntError,
    },

    #[error("failed to upsert indexed document {path}")]
    UpsertDocument {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to upsert indexed file {path}")]
    UpsertFile {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to delete indexed chunks for {path}")]
    DeleteFileChunks {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to insert indexed chunk {path}#{chunk_index}")]
    InsertFileChunk {
        path: String,
        chunk_index: u32,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to replace classifications for {path}")]
    ReplaceDirectoryClassifications {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to insert classification {label} for {path}")]
    InsertDirectoryClassification {
        path: String,
        label: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to delete indexed path tree {path}")]
    DeletePathTree {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to reset database content")]
    ResetDatabase {
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to count indexed documents")]
    CountDocuments {
        #[source]
        source: sqlx::Error,
    },

    #[error("document count was negative")]
    NegativeDocumentCount {
        #[source]
        source: TryFromIntError,
    },

    #[error("failed to look up indexed document {path}")]
    LookupDocument {
        path: String,
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to decode indexed document {path}")]
    InvalidDocumentSize {
        path: String,
        #[source]
        source: TryFromIntError,
    },

    #[error("failed to read directory documents")]
    ReadDirectoryDocuments {
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to read file chunk embeddings")]
    ReadFileChunks {
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to read directory classifications")]
    ReadDirectoryClassifications {
        #[source]
        source: sqlx::Error,
    },

    #[error("failed to read directory type counts")]
    ReadDirectoryTypeCounts {
        #[source]
        source: sqlx::Error,
    },

    #[error("embedding blob length {len} is not divisible by 4")]
    InvalidEmbeddingBlobLength { len: usize },
}
