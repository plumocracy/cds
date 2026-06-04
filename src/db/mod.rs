mod document;
mod error;
mod schema;
mod vector;

use std::fs;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

pub use document::{
    DirectoryClassification, DirectoryTypeCount, DocumentKind, FileChunkMatch, IndexedDocument,
    IndexedFile, IndexedFileChunk,
};
pub use error::DbError;
pub use vector::{decode_embedding, encode_embedding};

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug)]
pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| DbError::CreateDatabaseDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let connection = Connection::open(path).map_err(|source| DbError::OpenDatabase {
            path: path.to_path_buf(),
            source,
        })?;
        schema::migrate(&connection).map_err(|source| DbError::Migrate {
            source: Box::new(source),
        })?;
        Ok(Self { connection })
    }

    pub fn open_existing(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(DbError::MissingDatabase {
                path: path.to_path_buf(),
            });
        }

        let connection = Connection::open(path).map_err(|source| DbError::OpenDatabase {
            path: path.to_path_buf(),
            source,
        })?;
        schema::migrate(&connection).map_err(|source| DbError::Migrate {
            source: Box::new(source),
        })?;
        Ok(Self { connection })
    }

    pub fn open_in_memory() -> Result<Self> {
        let connection =
            Connection::open_in_memory().map_err(|source| DbError::OpenInMemory { source })?;
        schema::migrate(&connection).map_err(|source| DbError::Migrate {
            source: Box::new(source),
        })?;
        Ok(Self { connection })
    }

    pub fn upsert_document(&self, document: &IndexedDocument) -> Result<()> {
        self.connection
            .execute(
                "
                INSERT INTO indexed_documents (
                    path,
                    name,
                    kind,
                    parent_path,
                    searchable_text,
                    embedding,
                    embedding_dim,
                    metadata_fingerprint,
                    size_bytes,
                    created_unix_seconds,
                    modified_unix_seconds,
                    accessed_unix_seconds,
                    readonly,
                    indexed_unix_seconds
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                ON CONFLICT(path) DO UPDATE SET
                    name = excluded.name,
                    kind = excluded.kind,
                    parent_path = excluded.parent_path,
                    searchable_text = excluded.searchable_text,
                    embedding = excluded.embedding,
                    embedding_dim = excluded.embedding_dim,
                    metadata_fingerprint = excluded.metadata_fingerprint,
                    size_bytes = excluded.size_bytes,
                    created_unix_seconds = excluded.created_unix_seconds,
                    modified_unix_seconds = excluded.modified_unix_seconds,
                    accessed_unix_seconds = excluded.accessed_unix_seconds,
                    readonly = excluded.readonly,
                    indexed_unix_seconds = excluded.indexed_unix_seconds
                ",
                params![
                    document.path,
                    document.name,
                    document.kind.as_str(),
                    document.parent_path,
                    document.searchable_text,
                    encode_embedding(&document.embedding),
                    i64::try_from(document.embedding.len())
                        .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
                    document.metadata_fingerprint,
                    i64::try_from(document.size_bytes)
                        .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                    document.created_unix_seconds,
                    document.modified_unix_seconds,
                    document.accessed_unix_seconds,
                    document.readonly,
                    document.indexed_unix_seconds,
                ],
            )
            .map_err(|source| DbError::UpsertDocument {
                path: document.path.clone(),
                source,
            })?;

        Ok(())
    }

    pub fn upsert_file(&self, file: &IndexedFile) -> Result<()> {
        self.connection
            .execute(
                "
                INSERT INTO indexed_files (
                    path,
                    directory_path,
                    name,
                    extension,
                    size_bytes,
                    created_unix_seconds,
                    modified_unix_seconds,
                    accessed_unix_seconds,
                    readonly,
                    content_fingerprint,
                    indexed_unix_seconds
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                ON CONFLICT(path) DO UPDATE SET
                    directory_path = excluded.directory_path,
                    name = excluded.name,
                    extension = excluded.extension,
                    size_bytes = excluded.size_bytes,
                    created_unix_seconds = excluded.created_unix_seconds,
                    modified_unix_seconds = excluded.modified_unix_seconds,
                    accessed_unix_seconds = excluded.accessed_unix_seconds,
                    readonly = excluded.readonly,
                    content_fingerprint = excluded.content_fingerprint,
                    indexed_unix_seconds = excluded.indexed_unix_seconds
                ",
                params![
                    file.path,
                    file.directory_path,
                    file.name,
                    file.extension,
                    i64::try_from(file.size_bytes)
                        .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                    file.created_unix_seconds,
                    file.modified_unix_seconds,
                    file.accessed_unix_seconds,
                    file.readonly,
                    file.content_fingerprint,
                    file.indexed_unix_seconds,
                ],
            )
            .map_err(|source| DbError::UpsertFile {
                path: file.path.clone(),
                source,
            })?;

        Ok(())
    }

    pub fn replace_file_chunks(&self, file_path: &str, chunks: &[IndexedFileChunk]) -> Result<()> {
        self.connection
            .execute(
                "DELETE FROM indexed_file_chunks WHERE file_path = ?1",
                [file_path],
            )
            .map_err(|source| DbError::DeleteFileChunks {
                path: file_path.to_string(),
                source,
            })?;

        for chunk in chunks {
            self.connection
                .execute(
                    "
                    INSERT INTO indexed_file_chunks (
                        file_path,
                        directory_path,
                        chunk_index,
                        content,
                        embedding,
                        embedding_dim,
                        start_byte,
                        end_byte,
                        indexed_unix_seconds
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                    ",
                    params![
                        chunk.file_path,
                        chunk.directory_path,
                        i64::from(chunk.chunk_index),
                        chunk.content,
                        encode_embedding(&chunk.embedding),
                        i64::try_from(chunk.embedding.len())
                            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
                        i64::try_from(chunk.start_byte)
                            .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                        i64::try_from(chunk.end_byte)
                            .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                        chunk.indexed_unix_seconds,
                    ],
                )
                .map_err(|source| DbError::InsertFileChunk {
                    path: chunk.file_path.clone(),
                    chunk_index: chunk.chunk_index,
                    source,
                })?;
        }

        Ok(())
    }

    pub fn replace_directory_classifications(
        &self,
        directory_path: &str,
        classifications: &[DirectoryClassification],
    ) -> Result<()> {
        self.connection
            .execute(
                "DELETE FROM directory_classifications WHERE directory_path = ?1",
                [directory_path],
            )
            .map_err(|source| DbError::ReplaceDirectoryClassifications {
                path: directory_path.to_string(),
                source,
            })?;

        for classification in classifications {
            self.connection
                .execute(
                    "
                    INSERT INTO directory_classifications (
                        directory_path,
                        label,
                        confidence,
                        detector,
                        evidence_path,
                        evidence_summary,
                        detected_unix_seconds
                    ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                    ",
                    params![
                        classification.directory_path,
                        classification.label,
                        classification.confidence,
                        classification.detector,
                        classification.evidence_path,
                        classification.evidence_summary,
                        classification.detected_unix_seconds,
                    ],
                )
                .map_err(|source| DbError::InsertDirectoryClassification {
                    path: classification.directory_path.clone(),
                    label: classification.label.clone(),
                    source,
                })?;
        }

        Ok(())
    }

    pub fn delete_path_tree(&self, path: &str) -> Result<()> {
        let path_len = i64::try_from(path.len())
            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?;

        self.connection
            .execute(
                "
                DELETE FROM directory_classifications
                WHERE directory_path = ?1
                    OR (
                        length(directory_path) > ?2
                        AND substr(directory_path, 1, ?2) = ?1
                        AND substr(directory_path, ?2 + 1, 1) = '/'
                    )
                ",
                params![path, path_len],
            )
            .map_err(|source| DbError::DeletePathTree {
                path: path.to_string(),
                source,
            })?;

        self.connection
            .execute(
                "
                DELETE FROM indexed_file_chunks
                WHERE file_path = ?1
                    OR directory_path = ?1
                    OR (
                        length(file_path) > ?2
                        AND substr(file_path, 1, ?2) = ?1
                        AND substr(file_path, ?2 + 1, 1) = '/'
                    )
                    OR (
                        length(directory_path) > ?2
                        AND substr(directory_path, 1, ?2) = ?1
                        AND substr(directory_path, ?2 + 1, 1) = '/'
                    )
                ",
                params![path, path_len],
            )
            .map_err(|source| DbError::DeletePathTree {
                path: path.to_string(),
                source,
            })?;

        self.connection
            .execute(
                "
                DELETE FROM indexed_files
                WHERE path = ?1
                    OR directory_path = ?1
                    OR (
                        length(path) > ?2
                        AND substr(path, 1, ?2) = ?1
                        AND substr(path, ?2 + 1, 1) = '/'
                    )
                    OR (
                        length(directory_path) > ?2
                        AND substr(directory_path, 1, ?2) = ?1
                        AND substr(directory_path, ?2 + 1, 1) = '/'
                    )
                ",
                params![path, path_len],
            )
            .map_err(|source| DbError::DeletePathTree {
                path: path.to_string(),
                source,
            })?;

        self.connection
            .execute(
                "
                DELETE FROM indexed_documents
                WHERE path = ?1
                    OR (
                        length(path) > ?2
                        AND substr(path, 1, ?2) = ?1
                        AND substr(path, ?2 + 1, 1) = '/'
                    )
                ",
                params![path, path_len],
            )
            .map_err(|source| DbError::DeletePathTree {
                path: path.to_string(),
                source,
            })?;

        Ok(())
    }

    pub fn reset(&self) -> Result<()> {
        self.connection
            .execute_batch(
                "
                BEGIN;
                DELETE FROM directory_classifications;
                DELETE FROM indexed_file_chunks;
                DELETE FROM indexed_files;
                DELETE FROM indexed_documents;
                COMMIT;
                ",
            )
            .map_err(|source| DbError::ResetDatabase { source })?;

        Ok(())
    }

    pub fn document_count(&self) -> Result<u64> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM indexed_documents", [], |row| {
                row.get(0)
            })
            .map_err(|source| DbError::CountDocuments { source })?;
        u64::try_from(count).map_err(|source| DbError::NegativeDocumentCount { source })
    }

    pub fn get_document(&self, path: &str) -> Result<Option<IndexedDocument>> {
        let mut statement = self
            .connection
            .prepare(
                "
                SELECT
                    path,
                    name,
                    kind,
                    parent_path,
                    searchable_text,
                    embedding,
                    metadata_fingerprint,
                    size_bytes,
                    created_unix_seconds,
                    modified_unix_seconds,
                    accessed_unix_seconds,
                    readonly,
                    indexed_unix_seconds
                FROM indexed_documents
                WHERE path = ?1
                ",
            )
            .map_err(|source| DbError::PrepareDocumentLookup { source })?;

        let mut rows = statement
            .query_map([path], decode_document_row)
            .map_err(|source| DbError::LookupDocument {
                path: path.to_string(),
                source,
            })?;

        rows.next()
            .transpose()
            .map_err(|source| DbError::DecodeDocument {
                path: path.to_string(),
                source,
            })
    }

    pub fn directory_documents(&self) -> Result<Vec<IndexedDocument>> {
        let mut statement = self
            .connection
            .prepare(
                "
                SELECT
                    path,
                    name,
                    kind,
                    parent_path,
                    searchable_text,
                    embedding,
                    metadata_fingerprint,
                    size_bytes,
                    created_unix_seconds,
                    modified_unix_seconds,
                    accessed_unix_seconds,
                    readonly,
                    indexed_unix_seconds
                FROM indexed_documents
                WHERE kind = 'directory'
                ",
            )
            .map_err(|source| DbError::PrepareDirectoryDocumentScan { source })?;

        let rows = statement
            .query_map([], decode_document_row)
            .map_err(|source| DbError::ReadDirectoryDocuments { source })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryDocuments { source })
    }

    pub fn file_chunk_matches(&self) -> Result<Vec<FileChunkMatch>> {
        let mut statement = self
            .connection
            .prepare(
                "
                SELECT
                    chunk.file_path,
                    file.name,
                    chunk.directory_path,
                    chunk.content,
                    chunk.embedding,
                    file.modified_unix_seconds,
                    directory.modified_unix_seconds
                FROM indexed_file_chunks AS chunk
                INNER JOIN indexed_files AS file
                    ON file.path = chunk.file_path
                INNER JOIN indexed_documents AS directory
                    ON directory.path = chunk.directory_path
                WHERE directory.kind = 'directory'
                ",
            )
            .map_err(|source| DbError::ReadFileChunks { source })?;

        let rows = statement
            .query_map([], |row| {
                let embedding: Vec<u8> = row.get(4)?;
                Ok(FileChunkMatch {
                    file_path: row.get(0)?,
                    file_name: row.get(1)?,
                    directory_path: row.get(2)?,
                    content: row.get(3)?,
                    embedding: decode_embedding(&embedding).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Blob,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                err.to_string(),
                            )),
                        )
                    })?,
                    file_modified_unix_seconds: row.get(5)?,
                    directory_modified_unix_seconds: row.get(6)?,
                })
            })
            .map_err(|source| DbError::ReadFileChunks { source })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadFileChunks { source })
    }

    pub fn directory_classifications(
        &self,
        directory_path: &str,
    ) -> Result<Vec<DirectoryClassification>> {
        let mut statement = self
            .connection
            .prepare(
                "
                SELECT
                    directory_path,
                    label,
                    confidence,
                    detector,
                    evidence_path,
                    evidence_summary,
                    detected_unix_seconds
                FROM directory_classifications
                WHERE directory_path = ?1
                ",
            )
            .map_err(|source| DbError::ReadDirectoryClassifications { source })?;

        let rows = statement
            .query_map([directory_path], decode_classification_row)
            .map_err(|source| DbError::ReadDirectoryClassifications { source })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryClassifications { source })
    }

    pub fn ancestor_classifications(
        &self,
        directory_path: &str,
    ) -> Result<Vec<DirectoryClassification>> {
        let mut classifications = Vec::new();
        for ancestor in self.indexed_ancestors(directory_path)? {
            classifications.extend(self.directory_classifications(&ancestor)?);
        }
        Ok(classifications)
    }

    pub fn directory_type_counts(&self) -> Result<Vec<DirectoryTypeCount>> {
        let mut statement = self
            .connection
            .prepare(
                "
                SELECT label, COUNT(DISTINCT directory_path) AS directory_count
                FROM directory_classifications
                GROUP BY label
                ORDER BY directory_count DESC, label ASC
                ",
            )
            .map_err(|source| DbError::ReadDirectoryTypeCounts { source })?;

        let rows = statement
            .query_map([], |row| {
                let count: i64 = row.get(1)?;
                Ok(DirectoryTypeCount {
                    label: row.get(0)?,
                    count: u64::try_from(count).map_err(|err| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Integer,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                err.to_string(),
                            )),
                        )
                    })?,
                })
            })
            .map_err(|source| DbError::ReadDirectoryTypeCounts { source })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryTypeCounts { source })
    }

    pub fn general_indexed_directory(&self, path: &str) -> Result<String> {
        let ancestors = self.indexed_ancestors(path)?;

        if ancestors.len() >= 2 {
            return Ok(ancestors[ancestors.len() - 2].clone());
        }

        Ok(ancestors
            .first()
            .cloned()
            .unwrap_or_else(|| path.to_string()))
    }

    pub fn indexed_ancestors(&self, path: &str) -> Result<Vec<String>> {
        let mut current = Path::new(path);
        let mut ancestors = Vec::new();

        loop {
            let current_path = current.to_string_lossy();
            let exists = self
                .connection
                .query_row(
                    "
                    SELECT path
                    FROM indexed_documents
                    WHERE path = ?1 AND kind = 'directory'
                    ",
                    [current_path.as_ref()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|source| DbError::LookupDocument {
                    path: current_path.into_owned(),
                    source,
                })?;

            if let Some(path) = exists {
                ancestors.push(path);
            }

            let Some(parent) = current.parent() else {
                break;
            };
            current = parent;
        }

        Ok(ancestors)
    }
}

fn decode_classification_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DirectoryClassification> {
    Ok(DirectoryClassification {
        directory_path: row.get(0)?,
        label: row.get(1)?,
        confidence: row.get(2)?,
        detector: row.get(3)?,
        evidence_path: row.get(4)?,
        evidence_summary: row.get(5)?,
        detected_unix_seconds: row.get(6)?,
    })
}

fn decode_document_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedDocument> {
    let kind: String = row.get(2)?;
    let embedding: Vec<u8> = row.get(5)?;
    let size_bytes: i64 = row.get(7)?;
    Ok(IndexedDocument {
        path: row.get(0)?,
        name: row.get(1)?,
        kind: DocumentKind::from_db_value(&kind),
        parent_path: row.get(3)?,
        searchable_text: row.get(4)?,
        embedding: decode_embedding(&embedding).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Blob,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        metadata_fingerprint: row.get(6)?,
        size_bytes: u64::try_from(size_bytes).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Integer,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        created_unix_seconds: row.get(8)?,
        modified_unix_seconds: row.get(9)?,
        accessed_unix_seconds: row.get(10)?,
        readonly: row.get(11)?,
        indexed_unix_seconds: row.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upserts_and_reads_document() {
        let db = Database::open_in_memory().unwrap();
        let document = IndexedDocument {
            path: "/tmp/project".to_string(),
            name: "project".to_string(),
            kind: DocumentKind::Directory,
            parent_path: Some("/tmp".to_string()),
            searchable_text: "project readme cargo".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            metadata_fingerprint: "fingerprint".to_string(),
            size_bytes: 4096,
            created_unix_seconds: Some(10),
            modified_unix_seconds: 12,
            accessed_unix_seconds: Some(14),
            readonly: false,
            indexed_unix_seconds: 34,
        };

        db.upsert_document(&document).unwrap();

        assert_eq!(db.document_count().unwrap(), 1);
        assert_eq!(db.get_document("/tmp/project").unwrap(), Some(document));
    }

    #[test]
    fn migrates_v1_database_to_metadata_schema() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cds.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE indexed_documents (
                    path TEXT PRIMARY KEY NOT NULL,
                    kind TEXT NOT NULL CHECK (kind IN ('directory', 'file')),
                    parent_path TEXT,
                    searchable_text TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    embedding_dim INTEGER NOT NULL,
                    metadata_fingerprint TEXT NOT NULL,
                    modified_unix_seconds INTEGER NOT NULL,
                    indexed_unix_seconds INTEGER NOT NULL
                );
                PRAGMA user_version = 1;
                ",
            )
            .unwrap();
        drop(connection);

        let db = Database::open(&path).unwrap();
        let version: i64 = db
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();

        assert_eq!(version, 4);
        db.upsert_document(&IndexedDocument {
            path: "/tmp/project".to_string(),
            name: "project".to_string(),
            kind: DocumentKind::Directory,
            parent_path: Some("/tmp".to_string()),
            searchable_text: "project readme cargo".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            metadata_fingerprint: "fingerprint".to_string(),
            size_bytes: 4096,
            created_unix_seconds: Some(10),
            modified_unix_seconds: 12,
            accessed_unix_seconds: Some(14),
            readonly: true,
            indexed_unix_seconds: 34,
        })
        .unwrap();

        let document = db.get_document("/tmp/project").unwrap().unwrap();
        assert_eq!(document.name, "project");
        assert_eq!(document.size_bytes, 4096);
        assert_eq!(document.created_unix_seconds, Some(10));
        assert_eq!(document.accessed_unix_seconds, Some(14));
        assert!(document.readonly);
    }

    #[test]
    fn replaces_and_reads_directory_classifications() {
        let db = Database::open_in_memory().unwrap();
        let classification = DirectoryClassification {
            directory_path: "/tmp/project".to_string(),
            label: "rust project".to_string(),
            confidence: 0.98,
            detector: "cargo_toml_package".to_string(),
            evidence_path: Some("/tmp/project/Cargo.toml".to_string()),
            evidence_summary: "Cargo.toml contains [package]".to_string(),
            detected_unix_seconds: 100,
        };

        db.replace_directory_classifications("/tmp/project", std::slice::from_ref(&classification))
            .unwrap();
        assert_eq!(
            db.directory_classifications("/tmp/project").unwrap(),
            vec![classification]
        );

        db.replace_directory_classifications("/tmp/project", &[])
            .unwrap();
        assert!(
            db.directory_classifications("/tmp/project")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn counts_distinct_directories_by_type() {
        let db = Database::open_in_memory().unwrap();
        let rust_one = DirectoryClassification {
            directory_path: "/tmp/one".to_string(),
            label: "rust project".to_string(),
            confidence: 0.98,
            detector: "cargo".to_string(),
            evidence_path: None,
            evidence_summary: "Cargo.toml exists".to_string(),
            detected_unix_seconds: 100,
        };
        let rust_two = DirectoryClassification {
            directory_path: "/tmp/two".to_string(),
            label: "rust project".to_string(),
            confidence: 0.98,
            detector: "cargo".to_string(),
            evidence_path: None,
            evidence_summary: "Cargo.toml exists".to_string(),
            detected_unix_seconds: 100,
        };
        let chrome = DirectoryClassification {
            directory_path: "/tmp/three".to_string(),
            label: "chrome extension".to_string(),
            confidence: 1.0,
            detector: "manifest".to_string(),
            evidence_path: None,
            evidence_summary: "manifest.json exists".to_string(),
            detected_unix_seconds: 100,
        };

        db.replace_directory_classifications("/tmp/one", &[rust_one])
            .unwrap();
        db.replace_directory_classifications("/tmp/two", &[rust_two])
            .unwrap();
        db.replace_directory_classifications("/tmp/three", &[chrome])
            .unwrap();

        assert_eq!(
            db.directory_type_counts().unwrap(),
            vec![
                DirectoryTypeCount {
                    label: "rust project".to_string(),
                    count: 2,
                },
                DirectoryTypeCount {
                    label: "chrome extension".to_string(),
                    count: 1,
                },
            ]
        );
    }

    #[test]
    fn reset_clears_indexed_content() {
        let db = Database::open_in_memory().unwrap();
        db.upsert_document(&IndexedDocument {
            path: "/tmp/project".to_string(),
            name: "project".to_string(),
            kind: DocumentKind::Directory,
            parent_path: Some("/tmp".to_string()),
            searchable_text: "project readme cargo".to_string(),
            embedding: Vec::new(),
            metadata_fingerprint: "fingerprint".to_string(),
            size_bytes: 4096,
            created_unix_seconds: Some(10),
            modified_unix_seconds: 12,
            accessed_unix_seconds: Some(14),
            readonly: false,
            indexed_unix_seconds: 34,
        })
        .unwrap();
        db.replace_directory_classifications(
            "/tmp/project",
            &[DirectoryClassification {
                directory_path: "/tmp/project".to_string(),
                label: "rust project".to_string(),
                confidence: 0.98,
                detector: "cargo".to_string(),
                evidence_path: None,
                evidence_summary: "Cargo.toml exists".to_string(),
                detected_unix_seconds: 100,
            }],
        )
        .unwrap();

        db.reset().unwrap();

        assert_eq!(db.document_count().unwrap(), 0);
        assert!(db.directory_type_counts().unwrap().is_empty());
    }
}
