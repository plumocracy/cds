mod document;
mod error;
mod vector;

use std::collections::HashSet;
use std::ffi::{c_char, c_int};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{QueryBuilder, Row, Sqlite, SqlitePool, Transaction};

pub use document::{
    DirectoryClassification, DirectoryTypeCount, DocumentKind, FileChunkMatch, IndexedDocument,
    IndexedFile, IndexedFileChunk, ModifiedTimeRange,
};
pub use error::DbError;
pub use vector::{decode_embedding, encode_embedding};

pub type Result<T> = std::result::Result<T, DbError>;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");
const SQLITE_VEC_MAX_DIMENSIONS: usize = 8192;

type SqliteExtensionInit = unsafe extern "C" fn(
    db: *mut libsqlite3_sys::sqlite3,
    pz_err_msg: *mut *mut c_char,
    api: *const libsqlite3_sys::sqlite3_api_routines,
) -> c_int;

static SQLITE_VEC_REGISTRATION: OnceLock<std::result::Result<(), i32>> = OnceLock::new();

#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| DbError::CreateDatabaseDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let pool = open_file_pool(path, true).await?;
        migrate_pool(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_existing(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(DbError::MissingDatabase {
                path: path.to_path_buf(),
            });
        }

        let pool = open_file_pool(path, false).await?;
        migrate_pool(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn open_in_memory() -> Result<Self> {
        register_sqlite_vec_extension()?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|source| DbError::OpenInMemory { source })?;
        migrate_pool(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn current_revision(&self) -> Result<i64> {
        sqlx::query_scalar(
            "
            SELECT value
            FROM index_metadata
            WHERE key = 'revision'
            ",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|source| DbError::ReadIndexRevision { source })
    }

    pub async fn upsert_document(&self, document: &IndexedDocument) -> Result<()> {
        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| DbError::UpsertDocument {
                    path: document.path.clone(),
                    source,
                })?;

        sqlx::query(
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        )
        .bind(&document.path)
        .bind(&document.name)
        .bind(document.kind.as_str())
        .bind(&document.parent_path)
        .bind(&document.searchable_text)
        .bind(encode_embedding(&document.embedding))
        .bind(
            i64::try_from(document.embedding.len())
                .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
        )
        .bind(&document.metadata_fingerprint)
        .bind(
            i64::try_from(document.size_bytes)
                .map_err(|source| DbError::MetadataSizeOverflow { source })?,
        )
        .bind(document.created_unix_seconds)
        .bind(document.modified_unix_seconds)
        .bind(document.accessed_unix_seconds)
        .bind(document.readonly)
        .bind(document.indexed_unix_seconds)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::UpsertDocument {
            path: document.path.clone(),
            source,
        })?;

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::UpsertDocument {
                path: document.path.clone(),
                source,
            })?;

        Ok(())
    }

    pub async fn upsert_file(&self, file: &IndexedFile) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| DbError::UpsertFile {
                path: file.path.clone(),
                source,
            })?;

        sqlx::query(
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        )
        .bind(&file.path)
        .bind(&file.directory_path)
        .bind(&file.name)
        .bind(&file.extension)
        .bind(
            i64::try_from(file.size_bytes)
                .map_err(|source| DbError::MetadataSizeOverflow { source })?,
        )
        .bind(file.created_unix_seconds)
        .bind(file.modified_unix_seconds)
        .bind(file.accessed_unix_seconds)
        .bind(file.readonly)
        .bind(&file.content_fingerprint)
        .bind(file.indexed_unix_seconds)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::UpsertFile {
            path: file.path.clone(),
            source,
        })?;

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::UpsertFile {
                path: file.path.clone(),
                source,
            })?;

        Ok(())
    }

    pub async fn upsert_directories_with_classifications(
        &self,
        directories: &[(&IndexedDocument, &[DirectoryClassification])],
    ) -> Result<()> {
        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| DbError::UpsertDocument {
                    path: "<directory batch>".to_string(),
                    source,
                })?;

        for (document, classifications) in directories {
            sqlx::query(
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
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            )
            .bind(&document.path)
            .bind(&document.name)
            .bind(document.kind.as_str())
            .bind(&document.parent_path)
            .bind(&document.searchable_text)
            .bind(encode_embedding(&document.embedding))
            .bind(
                i64::try_from(document.embedding.len())
                    .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
            )
            .bind(&document.metadata_fingerprint)
            .bind(
                i64::try_from(document.size_bytes)
                    .map_err(|source| DbError::MetadataSizeOverflow { source })?,
            )
            .bind(document.created_unix_seconds)
            .bind(document.modified_unix_seconds)
            .bind(document.accessed_unix_seconds)
            .bind(document.readonly)
            .bind(document.indexed_unix_seconds)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::UpsertDocument {
                path: document.path.clone(),
                source,
            })?;

            sqlx::query("DELETE FROM directory_classifications WHERE directory_path = ?")
                .bind(&document.path)
                .execute(&mut *transaction)
                .await
                .map_err(|source| DbError::ReplaceDirectoryClassifications {
                    path: document.path.clone(),
                    source,
                })?;

            for classification in *classifications {
                sqlx::query(
                    "
                    INSERT INTO directory_classifications (
                        directory_path,
                        label,
                        confidence,
                        detector,
                        evidence_path,
                        evidence_summary,
                        detected_unix_seconds
                    ) VALUES (?, ?, ?, ?, ?, ?, ?)
                    ",
                )
                .bind(&classification.directory_path)
                .bind(&classification.label)
                .bind(f64::from(classification.confidence))
                .bind(&classification.detector)
                .bind(&classification.evidence_path)
                .bind(&classification.evidence_summary)
                .bind(classification.detected_unix_seconds)
                .execute(&mut *transaction)
                .await
                .map_err(|source| DbError::InsertDirectoryClassification {
                    path: classification.directory_path.clone(),
                    label: classification.label.clone(),
                    source,
                })?;
            }
        }

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::CommitFileBatch { source })?;

        Ok(())
    }

    pub async fn replace_file_chunks(
        &self,
        file_path: &str,
        chunks: &[IndexedFileChunk],
    ) -> Result<()> {
        ensure_file_chunk_vector_tables_for_chunks(&self.pool, chunks).await?;

        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| DbError::DeleteFileChunks {
                    path: file_path.to_string(),
                    source,
                })?;

        delete_current_file_chunk_vectors_for_file(&mut transaction, file_path).await?;
        let vector_file = indexed_file_for_vector_metadata(&mut transaction, file_path).await?;

        sqlx::query("DELETE FROM indexed_file_chunks WHERE file_path = ?")
            .bind(file_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::DeleteFileChunks {
                path: file_path.to_string(),
                source,
            })?;

        for chunk in chunks {
            sqlx::query(
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
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(&chunk.file_path)
            .bind(&chunk.directory_path)
            .bind(i64::from(chunk.chunk_index))
            .bind(&chunk.content)
            .bind(encode_embedding(&chunk.embedding))
            .bind(
                i64::try_from(chunk.embedding.len())
                    .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
            )
            .bind(
                i64::try_from(chunk.start_byte)
                    .map_err(|source| DbError::MetadataSizeOverflow { source })?,
            )
            .bind(
                i64::try_from(chunk.end_byte)
                    .map_err(|source| DbError::MetadataSizeOverflow { source })?,
            )
            .bind(chunk.indexed_unix_seconds)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::InsertFileChunk {
                path: chunk.file_path.clone(),
                chunk_index: chunk.chunk_index,
                source,
            })?;

            let fallback_file;
            let vector_file = if let Some(file) = &vector_file {
                file
            } else {
                fallback_file = IndexedFile {
                    path: chunk.file_path.clone(),
                    directory_path: chunk.directory_path.clone(),
                    name: Path::new(&chunk.file_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or(&chunk.file_path)
                        .to_string(),
                    extension: Path::new(&chunk.file_path)
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map(|extension| extension.to_string()),
                    size_bytes: chunk.end_byte.saturating_sub(chunk.start_byte),
                    created_unix_seconds: None,
                    modified_unix_seconds: chunk.indexed_unix_seconds,
                    accessed_unix_seconds: None,
                    readonly: false,
                    content_fingerprint: String::new(),
                    indexed_unix_seconds: chunk.indexed_unix_seconds,
                };
                &fallback_file
            };
            insert_current_file_chunk_vector(&mut transaction, vector_file, chunk).await?;
        }

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::CommitFileBatch { source })?;

        Ok(())
    }

    pub async fn delete_current_file(&self, file_path: &str) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| DbError::DeleteFile {
                path: file_path.to_string(),
                source,
            })?;

        delete_current_file_chunk_vectors_for_file(&mut transaction, file_path).await?;

        sqlx::query("DELETE FROM indexed_file_chunks WHERE file_path = ?")
            .bind(file_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::DeleteFileChunks {
                path: file_path.to_string(),
                source,
            })?;

        sqlx::query("DELETE FROM indexed_files WHERE path = ?")
            .bind(file_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::DeleteFile {
                path: file_path.to_string(),
                source,
            })?;

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::DeleteFile {
                path: file_path.to_string(),
                source,
            })?;

        Ok(())
    }

    pub async fn upsert_files_with_chunks(
        &self,
        files: &[(&IndexedFile, &[IndexedFileChunk])],
    ) -> Result<()> {
        for (_, chunks) in files {
            ensure_file_chunk_vector_tables_for_chunks(&self.pool, chunks).await?;
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| DbError::UpsertFile {
                path: "<batch>".to_string(),
                source,
            })?;

        for (file, chunks) in files {
            sqlx::query(
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
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            )
            .bind(&file.path)
            .bind(&file.directory_path)
            .bind(&file.name)
            .bind(&file.extension)
            .bind(
                i64::try_from(file.size_bytes)
                    .map_err(|source| DbError::MetadataSizeOverflow { source })?,
            )
            .bind(file.created_unix_seconds)
            .bind(file.modified_unix_seconds)
            .bind(file.accessed_unix_seconds)
            .bind(file.readonly)
            .bind(&file.content_fingerprint)
            .bind(file.indexed_unix_seconds)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::UpsertFile {
                path: file.path.clone(),
                source,
            })?;

            delete_current_file_chunk_vectors_for_file(&mut transaction, &file.path).await?;

            sqlx::query("DELETE FROM indexed_file_chunks WHERE file_path = ?")
                .bind(&file.path)
                .execute(&mut *transaction)
                .await
                .map_err(|source| DbError::DeleteFileChunks {
                    path: file.path.clone(),
                    source,
                })?;

            for chunk in *chunks {
                sqlx::query(
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
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ",
                )
                .bind(&chunk.file_path)
                .bind(&chunk.directory_path)
                .bind(i64::from(chunk.chunk_index))
                .bind(&chunk.content)
                .bind(encode_embedding(&chunk.embedding))
                .bind(
                    i64::try_from(chunk.embedding.len())
                        .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
                )
                .bind(
                    i64::try_from(chunk.start_byte)
                        .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                )
                .bind(
                    i64::try_from(chunk.end_byte)
                        .map_err(|source| DbError::MetadataSizeOverflow { source })?,
                )
                .bind(chunk.indexed_unix_seconds)
                .execute(&mut *transaction)
                .await
                .map_err(|source| DbError::InsertFileChunk {
                    path: chunk.file_path.clone(),
                    chunk_index: chunk.chunk_index,
                    source,
                })?;

                insert_current_file_chunk_vector(&mut transaction, file, chunk).await?;
                let history_id = insert_file_chunk_history(&mut transaction, file, chunk).await?;
                insert_history_file_chunk_vector(&mut transaction, history_id, file, chunk).await?;
            }
        }

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::CommitFileBatch { source })?;

        Ok(())
    }

    pub async fn replace_directory_classifications(
        &self,
        directory_path: &str,
        classifications: &[DirectoryClassification],
    ) -> Result<()> {
        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| DbError::ReplaceDirectoryClassifications {
                    path: directory_path.to_string(),
                    source,
                })?;

        sqlx::query("DELETE FROM directory_classifications WHERE directory_path = ?")
            .bind(directory_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ReplaceDirectoryClassifications {
                path: directory_path.to_string(),
                source,
            })?;

        for classification in classifications {
            sqlx::query(
                "
                INSERT INTO directory_classifications (
                    directory_path,
                    label,
                    confidence,
                    detector,
                    evidence_path,
                    evidence_summary,
                    detected_unix_seconds
                ) VALUES (?, ?, ?, ?, ?, ?, ?)
                ",
            )
            .bind(&classification.directory_path)
            .bind(&classification.label)
            .bind(f64::from(classification.confidence))
            .bind(&classification.detector)
            .bind(&classification.evidence_path)
            .bind(&classification.evidence_summary)
            .bind(classification.detected_unix_seconds)
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::InsertDirectoryClassification {
                path: classification.directory_path.clone(),
                label: classification.label.clone(),
                source,
            })?;
        }

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::ReplaceDirectoryClassifications {
                path: directory_path.to_string(),
                source,
            })?;

        Ok(())
    }

    pub async fn delete_path_tree(&self, path: &str) -> Result<()> {
        let path_len = i64::try_from(path.len())
            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?;
        let mut transaction =
            self.pool
                .begin()
                .await
                .map_err(|source| DbError::DeletePathTree {
                    path: path.to_string(),
                    source,
                })?;

        delete_file_chunk_vectors_for_path_tree(&mut transaction, path, path_len).await?;

        sqlx::query(
            "
            DELETE FROM directory_classifications
            WHERE directory_path = ?
                OR (
                    length(directory_path) > ?
                    AND substr(directory_path, 1, ?) = ?
                    AND substr(directory_path, ? + 1, 1) = '/'
                )
            ",
        )
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        sqlx::query(
            "
            DELETE FROM indexed_file_chunk_history
            WHERE file_path = ?
                OR directory_path = ?
                OR (
                    length(file_path) > ?
                    AND substr(file_path, 1, ?) = ?
                    AND substr(file_path, ? + 1, 1) = '/'
                )
                OR (
                    length(directory_path) > ?
                    AND substr(directory_path, 1, ?) = ?
                    AND substr(directory_path, ? + 1, 1) = '/'
                )
            ",
        )
        .bind(path)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        sqlx::query(
            "
            DELETE FROM indexed_file_chunks
            WHERE file_path = ?
                OR directory_path = ?
                OR (
                    length(file_path) > ?
                    AND substr(file_path, 1, ?) = ?
                    AND substr(file_path, ? + 1, 1) = '/'
                )
                OR (
                    length(directory_path) > ?
                    AND substr(directory_path, 1, ?) = ?
                    AND substr(directory_path, ? + 1, 1) = '/'
                )
            ",
        )
        .bind(path)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        sqlx::query(
            "
            DELETE FROM indexed_files
            WHERE path = ?
                OR directory_path = ?
                OR (
                    length(path) > ?
                    AND substr(path, 1, ?) = ?
                    AND substr(path, ? + 1, 1) = '/'
                )
                OR (
                    length(directory_path) > ?
                    AND substr(directory_path, 1, ?) = ?
                    AND substr(directory_path, ? + 1, 1) = '/'
                )
            ",
        )
        .bind(path)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        sqlx::query(
            "
            DELETE FROM indexed_documents
            WHERE path = ?
                OR (
                    length(path) > ?
                    AND substr(path, 1, ?) = ?
                    AND substr(path, ? + 1, 1) = '/'
                )
            ",
        )
        .bind(path)
        .bind(path_len)
        .bind(path_len)
        .bind(path)
        .bind(path_len)
        .execute(&mut *transaction)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::DeletePathTree {
                path: path.to_string(),
                source,
            })?;

        Ok(())
    }

    pub async fn reset(&self) -> Result<()> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;

        sqlx::query("PRAGMA secure_delete = ON")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        delete_all_file_chunk_vectors(&mut transaction).await?;
        sqlx::query("DELETE FROM directory_classifications")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_file_chunks")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_file_chunk_history")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_files")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_documents")
            .execute(&mut *transaction)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;

        bump_index_revision(&mut transaction).await?;
        transaction
            .commit()
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;

        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("VACUUM")
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;

        Ok(())
    }

    pub async fn document_count(&self) -> Result<u64> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM indexed_documents")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| DbError::CountDocuments { source })?;
        u64::try_from(count).map_err(|source| DbError::NegativeDocumentCount { source })
    }

    pub async fn get_document(&self, path: &str) -> Result<Option<IndexedDocument>> {
        let row = sqlx::query(
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
            WHERE path = ?
            ",
        )
        .bind(path)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| DbError::LookupDocument {
            path: path.to_string(),
            source,
        })?;

        row.map(decode_document_row)
            .transpose()
            .map_err(|source| DbError::LookupDocument {
                path: path.to_string(),
                source,
            })
    }

    pub async fn directory_documents(&self) -> Result<Vec<IndexedDocument>> {
        let rows = sqlx::query(
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
        .fetch_all(&self.pool)
        .await
        .map_err(|source| DbError::ReadDirectoryDocuments { source })?;

        rows.into_iter()
            .map(decode_document_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryDocuments { source })
    }

    pub async fn directory_search_documents(&self) -> Result<Vec<IndexedDocument>> {
        let rows = sqlx::query(
            "
            SELECT
                path,
                name,
                kind,
                parent_path,
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
        .fetch_all(&self.pool)
        .await
        .map_err(|source| DbError::ReadDirectoryDocuments { source })?;

        rows.into_iter()
            .map(decode_directory_search_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryDocuments { source })
    }

    pub async fn directory_candidates_by_terms(
        &self,
        terms: &[String],
    ) -> Result<Vec<IndexedDocument>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
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
            WHERE kind = 'directory' AND (
            ",
        );
        push_directory_term_filter(&mut query, terms);
        query.push(") ORDER BY path");

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| DbError::ReadDirectoryDocuments { source })?;

        rows.into_iter()
            .map(decode_document_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryDocuments { source })
    }

    pub async fn directory_search_candidates_by_terms(
        &self,
        terms: &[String],
    ) -> Result<Vec<IndexedDocument>> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let mut query = QueryBuilder::<Sqlite>::new(
            "
            SELECT
                path,
                name,
                kind,
                parent_path,
                size_bytes,
                created_unix_seconds,
                modified_unix_seconds,
                accessed_unix_seconds,
                readonly,
                indexed_unix_seconds
            FROM indexed_documents
            WHERE kind = 'directory' AND (
            ",
        );
        push_directory_term_filter(&mut query, terms);
        query.push(") ORDER BY path");

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| DbError::ReadDirectoryDocuments { source })?;

        rows.into_iter()
            .map(decode_directory_search_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryDocuments { source })
    }

    pub async fn nearest_file_chunk_matches_with_modified_range(
        &self,
        query_embedding: &[f32],
        modified_range: Option<ModifiedTimeRange>,
        limit: usize,
    ) -> Result<Vec<FileChunkMatch>> {
        if query_embedding.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let embedding_dim = i64::try_from(query_embedding.len())
            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?;
        let vector_ref_count: i64 = sqlx::query_scalar(
            "
            SELECT COUNT(*)
            FROM indexed_file_chunk_vector_refs
            WHERE embedding_dim = ?
            ",
        )
        .bind(embedding_dim)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

        if vector_ref_count == 0 {
            return self
                .file_chunk_matches_with_modified_range(modified_range)
                .await;
        }

        ensure_file_chunk_vector_table(&self.pool, query_embedding.len()).await?;

        let table_name = file_chunk_vector_table_name(query_embedding.len())?;
        let limit =
            i64::try_from(limit).map_err(|source| DbError::MetadataSizeOverflow { source })?;
        let mut query = QueryBuilder::<Sqlite>::new(
            "
            WITH vector_matches AS (
                SELECT
                    rowid AS vector_id,
                    distance
                FROM
            ",
        );
        query.push(table_name);
        query.push(" WHERE embedding MATCH ");
        query.push_bind(encode_embedding(query_embedding));
        query.push(" AND k = ");
        query.push_bind(limit);
        if let Some(range) = modified_range {
            query.push(" AND ");
            push_modified_time_filter(&mut query, "file_modified_unix_seconds", range);
        }
        query.push(
            "
            )
            SELECT
                chunk.file_path,
                file.name AS file_name,
                chunk.directory_path,
                chunk.content,
                chunk.embedding,
                1 AS is_current,
                file.modified_unix_seconds AS file_modified_unix_seconds,
                directory.modified_unix_seconds AS directory_modified_unix_seconds,
                vector_matches.distance AS vector_distance
            FROM vector_matches
            INNER JOIN indexed_file_chunk_vector_refs AS ref
                ON ref.vector_id = vector_matches.vector_id
                    AND ref.source = 'current'
            INNER JOIN indexed_file_chunks AS chunk
                ON chunk.file_path = ref.file_path
                    AND chunk.chunk_index = ref.chunk_index
            INNER JOIN indexed_files AS file
                ON file.path = chunk.file_path
            INNER JOIN indexed_documents AS directory
                ON directory.path = chunk.directory_path
            WHERE directory.kind = 'directory'
            UNION ALL
            SELECT
                history.file_path,
                history.file_name,
                history.directory_path,
                history.content,
                history.embedding,
                0 AS is_current,
                history.file_modified_unix_seconds,
                history.directory_modified_unix_seconds,
                vector_matches.distance AS vector_distance
            FROM vector_matches
            INNER JOIN indexed_file_chunk_vector_refs AS ref
                ON ref.vector_id = vector_matches.vector_id
                    AND ref.source = 'history'
            INNER JOIN indexed_file_chunk_history AS history
                ON history.id = ref.history_id
            ORDER BY vector_distance ASC
            ",
        );

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| DbError::ReadFileChunks { source })?;

        rows.into_iter()
            .map(decode_file_chunk_match_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadFileChunks { source })
    }

    pub async fn file_chunk_matches(&self) -> Result<Vec<FileChunkMatch>> {
        self.file_chunk_matches_with_modified_range(None).await
    }

    pub async fn file_chunk_matches_with_modified_range(
        &self,
        modified_range: Option<ModifiedTimeRange>,
    ) -> Result<Vec<FileChunkMatch>> {
        let mut query = QueryBuilder::<Sqlite>::new(
            "
            SELECT
                file_path,
                file_name,
                directory_path,
                content,
                embedding,
                0 AS is_current,
                file_modified_unix_seconds,
                directory_modified_unix_seconds
            FROM indexed_file_chunk_history
            ",
        );
        if let Some(range) = modified_range {
            query.push(" WHERE ");
            push_modified_time_filter(&mut query, "file_modified_unix_seconds", range);
        }
        query.push(
            "
            UNION ALL
            SELECT
                chunk.file_path,
                file.name AS file_name,
                chunk.directory_path,
                chunk.content,
                chunk.embedding,
                1 AS is_current,
                file.modified_unix_seconds AS file_modified_unix_seconds,
                directory.modified_unix_seconds AS directory_modified_unix_seconds
            FROM indexed_file_chunks AS chunk
            INNER JOIN indexed_files AS file
                ON file.path = chunk.file_path
            INNER JOIN indexed_documents AS directory
                ON directory.path = chunk.directory_path
            WHERE directory.kind = 'directory'
            ",
        );
        if let Some(range) = modified_range {
            query.push(" AND ");
            push_modified_time_filter(&mut query, "file.modified_unix_seconds", range);
        }

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| DbError::ReadFileChunks { source })?;

        rows.into_iter()
            .map(decode_file_chunk_match_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadFileChunks { source })
    }

    pub async fn file_chunk_matches_in_directory_trees(
        &self,
        directory_paths: &[String],
    ) -> Result<Vec<FileChunkMatch>> {
        self.file_chunk_matches_in_directory_trees_with_modified_range(directory_paths, None)
            .await
    }

    pub async fn file_chunk_matches_in_directory_trees_with_modified_range(
        &self,
        directory_paths: &[String],
        modified_range: Option<ModifiedTimeRange>,
    ) -> Result<Vec<FileChunkMatch>> {
        if directory_paths.is_empty() {
            return Ok(Vec::new());
        }
        let directory_paths = unique_directory_paths(directory_paths);

        let mut query = QueryBuilder::<Sqlite>::new(
            "
            WITH directory_tree_roots(path, child_pattern) AS (
                VALUES
            ",
        );
        push_directory_tree_root_values(&mut query, &directory_paths);
        query.push(
            "
            )
            SELECT
                file_path,
                file_name,
                directory_path,
                content,
                embedding,
                0 AS is_current,
                file_modified_unix_seconds,
                directory_modified_unix_seconds
            FROM indexed_file_chunk_history
            WHERE
            ",
        );
        push_directory_tree_exists_filter(&mut query, "directory_path");
        if let Some(range) = modified_range {
            query.push(" AND ");
            push_modified_time_filter(&mut query, "file_modified_unix_seconds", range);
        }
        query.push(
            "
            UNION ALL
            SELECT
                chunk.file_path,
                file.name AS file_name,
                chunk.directory_path,
                chunk.content,
                chunk.embedding,
                1 AS is_current,
                file.modified_unix_seconds AS file_modified_unix_seconds,
                directory.modified_unix_seconds AS directory_modified_unix_seconds
            FROM indexed_file_chunks AS chunk
            INNER JOIN indexed_files AS file
                ON file.path = chunk.file_path
            INNER JOIN indexed_documents AS directory
                ON directory.path = chunk.directory_path
            WHERE directory.kind = 'directory' AND
            ",
        );
        push_directory_tree_exists_filter(&mut query, "chunk.directory_path");
        if let Some(range) = modified_range {
            query.push(" AND ");
            push_modified_time_filter(&mut query, "file.modified_unix_seconds", range);
        }

        let rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(|source| DbError::ReadFileChunks { source })?;

        rows.into_iter()
            .map(decode_file_chunk_match_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadFileChunks { source })
    }

    pub async fn directory_classifications(
        &self,
        directory_path: &str,
    ) -> Result<Vec<DirectoryClassification>> {
        let rows = sqlx::query(
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
            WHERE directory_path = ?
            ",
        )
        .bind(directory_path)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| DbError::ReadDirectoryClassifications { source })?;

        rows.into_iter()
            .map(decode_classification_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryClassifications { source })
    }

    pub async fn ancestor_classifications(
        &self,
        directory_path: &str,
    ) -> Result<Vec<DirectoryClassification>> {
        let mut classifications = Vec::new();
        for ancestor in self.indexed_ancestors(directory_path).await? {
            classifications.extend(self.directory_classifications(&ancestor).await?);
        }
        Ok(classifications)
    }

    pub async fn directory_type_counts(&self) -> Result<Vec<DirectoryTypeCount>> {
        let rows = sqlx::query(
            "
            SELECT label, COUNT(DISTINCT directory_path) AS directory_count
            FROM directory_classifications
            GROUP BY label
            ORDER BY directory_count DESC, label ASC
            ",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| DbError::ReadDirectoryTypeCounts { source })?;

        rows.into_iter()
            .map(decode_directory_type_count_row)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|source| DbError::ReadDirectoryTypeCounts { source })
    }

    pub async fn general_indexed_directory(&self, path: &str) -> Result<String> {
        let ancestors = self.indexed_ancestors(path).await?;

        if ancestors.len() >= 2 {
            return Ok(ancestors[ancestors.len() - 2].clone());
        }

        Ok(ancestors
            .first()
            .cloned()
            .unwrap_or_else(|| path.to_string()))
    }

    pub async fn indexed_ancestors(&self, path: &str) -> Result<Vec<String>> {
        let mut current = Path::new(path);
        let mut ancestors = Vec::new();

        loop {
            let current_path = current.to_string_lossy();
            let exists: Option<String> = sqlx::query_scalar(
                "
                SELECT path
                FROM indexed_documents
                WHERE path = ? AND kind = 'directory'
                ",
            )
            .bind(current_path.as_ref())
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| DbError::LookupDocument {
                path: current_path.to_string(),
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

async fn open_file_pool(path: &Path, create_if_missing: bool) -> Result<SqlitePool> {
    register_sqlite_vec_extension()?;
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing);

    SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .map_err(|source| DbError::OpenDatabase {
            path: path.to_path_buf(),
            source,
        })
}

async fn migrate_pool(pool: &SqlitePool) -> Result<()> {
    prepare_legacy_schema(pool).await?;
    MIGRATOR
        .run(pool)
        .await
        .map_err(|source| DbError::Migrate { source })?;
    sqlx::query("PRAGMA user_version = 8")
        .execute(pool)
        .await
        .map_err(|source| DbError::PrepareLegacyMigration { source })?;
    backfill_file_chunk_vector_index(pool).await?;
    Ok(())
}

fn register_sqlite_vec_extension() -> Result<()> {
    let registration = SQLITE_VEC_REGISTRATION.get_or_init(|| {
        // sqlite-vec exposes its init symbol without the SQLite extension signature,
        // so registration follows the crate's documented rusqlite pattern.
        let entry = unsafe {
            std::mem::transmute::<*const (), SqliteExtensionInit>(
                sqlite_vec::sqlite3_vec_init as *const (),
            )
        };
        let status = unsafe { libsqlite3_sys::sqlite3_auto_extension(Some(entry)) };
        if status == libsqlite3_sys::SQLITE_OK {
            Ok(())
        } else {
            Err(status)
        }
    });

    registration
        .as_ref()
        .copied()
        .map_err(|code| DbError::RegisterSqliteVec { code: *code })
}

fn file_chunk_vector_table_name(dimension: usize) -> Result<String> {
    if dimension == 0 || dimension > SQLITE_VEC_MAX_DIMENSIONS {
        return Err(DbError::InvalidEmbeddingDimension { dimension });
    }

    Ok(format!("indexed_file_chunk_vec_{dimension}"))
}

fn is_file_chunk_vector_table_name(name: &str) -> bool {
    let Some(dimension) = name.strip_prefix("indexed_file_chunk_vec_") else {
        return false;
    };
    !dimension.is_empty() && dimension.chars().all(|ch| ch.is_ascii_digit())
}

async fn ensure_file_chunk_vector_table(pool: &SqlitePool, dimension: usize) -> Result<()> {
    let table_name = file_chunk_vector_table_name(dimension)?;
    let sql = format!(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS {table_name}
        USING vec0(
            embedding float[{dimension}] distance_metric=cosine,
            file_modified_unix_seconds integer
        )
        "
    );
    sqlx::query(&sql)
        .execute(pool)
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    Ok(())
}

async fn ensure_file_chunk_vector_tables_for_chunks(
    pool: &SqlitePool,
    chunks: &[IndexedFileChunk],
) -> Result<()> {
    let dimensions = chunks
        .iter()
        .map(|chunk| chunk.embedding.len())
        .filter(|dimension| *dimension > 0)
        .collect::<HashSet<_>>();

    for dimension in dimensions {
        ensure_file_chunk_vector_table(pool, dimension).await?;
    }

    Ok(())
}

#[derive(Debug)]
struct FileChunkVectorRef {
    vector_id: i64,
    embedding_dim: usize,
}

#[derive(Debug)]
struct BackfillFileChunkVector {
    source: &'static str,
    history_id: i64,
    file_path: String,
    directory_path: String,
    chunk_index: i64,
    embedding: Vec<f32>,
    file_modified_unix_seconds: i64,
    directory_modified_unix_seconds: i64,
    indexed_unix_seconds: i64,
}

struct FileChunkVectorWrite<'a> {
    source: &'static str,
    history_id: i64,
    file_path: &'a str,
    directory_path: &'a str,
    chunk_index: i64,
    embedding: &'a [f32],
    file_modified_unix_seconds: i64,
    directory_modified_unix_seconds: i64,
    indexed_unix_seconds: i64,
}

async fn backfill_file_chunk_vector_index(pool: &SqlitePool) -> Result<()> {
    let current_rows = sqlx::query(
        "
        SELECT
            'current' AS source,
            0 AS history_id,
            chunk.file_path,
            chunk.directory_path,
            chunk.chunk_index,
            chunk.embedding,
            file.modified_unix_seconds AS file_modified_unix_seconds,
            directory.modified_unix_seconds AS directory_modified_unix_seconds,
            chunk.indexed_unix_seconds
        FROM indexed_file_chunks AS chunk
        INNER JOIN indexed_files AS file
            ON file.path = chunk.file_path
        INNER JOIN indexed_documents AS directory
            ON directory.path = chunk.directory_path
        LEFT JOIN indexed_file_chunk_vector_refs AS ref
            ON ref.source = 'current'
                AND ref.history_id = 0
                AND ref.file_path = chunk.file_path
                AND ref.chunk_index = chunk.chunk_index
        WHERE ref.vector_id IS NULL
        ",
    )
    .fetch_all(pool)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    let history_rows = sqlx::query(
        "
        SELECT
            'history' AS source,
            history.id AS history_id,
            history.file_path,
            history.directory_path,
            history.chunk_index,
            history.embedding,
            history.file_modified_unix_seconds,
            history.directory_modified_unix_seconds,
            history.indexed_unix_seconds
        FROM indexed_file_chunk_history AS history
        LEFT JOIN indexed_file_chunk_vector_refs AS ref
            ON ref.source = 'history'
                AND ref.history_id = history.id
                AND ref.file_path = history.file_path
                AND ref.chunk_index = history.chunk_index
        WHERE ref.vector_id IS NULL
        ",
    )
    .fetch_all(pool)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    let mut rows = current_rows
        .into_iter()
        .map(decode_backfill_file_chunk_vector_row)
        .chain(
            history_rows
                .into_iter()
                .map(decode_backfill_file_chunk_vector_row),
        )
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    rows.retain(|row| !row.embedding.is_empty());
    if rows.is_empty() {
        return Ok(());
    }

    let dimensions = rows
        .iter()
        .map(|row| row.embedding.len())
        .collect::<HashSet<_>>();
    for dimension in dimensions {
        ensure_file_chunk_vector_table(pool, dimension).await?;
    }

    let mut transaction = pool
        .begin()
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    for row in rows {
        insert_file_chunk_vector_ref(
            &mut transaction,
            FileChunkVectorWrite {
                source: row.source,
                history_id: row.history_id,
                file_path: &row.file_path,
                directory_path: &row.directory_path,
                chunk_index: row.chunk_index,
                embedding: &row.embedding,
                file_modified_unix_seconds: row.file_modified_unix_seconds,
                directory_modified_unix_seconds: row.directory_modified_unix_seconds,
                indexed_unix_seconds: row.indexed_unix_seconds,
            },
        )
        .await?;
    }
    transaction
        .commit()
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    Ok(())
}

async fn delete_current_file_chunk_vectors_for_file(
    transaction: &mut Transaction<'_, Sqlite>,
    file_path: &str,
) -> Result<()> {
    let rows = sqlx::query(
        "
        SELECT vector_id, embedding_dim
        FROM indexed_file_chunk_vector_refs
        WHERE source = 'current' AND file_path = ?
        ",
    )
    .bind(file_path)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    delete_file_chunk_vector_refs(transaction, decode_file_chunk_vector_refs(rows)?).await
}

async fn delete_file_chunk_vectors_for_path_tree(
    transaction: &mut Transaction<'_, Sqlite>,
    path: &str,
    path_len: i64,
) -> Result<()> {
    let rows = sqlx::query(
        "
        SELECT vector_id, embedding_dim
        FROM indexed_file_chunk_vector_refs
        WHERE file_path = ?
            OR directory_path = ?
            OR (
                length(file_path) > ?
                AND substr(file_path, 1, ?) = ?
                AND substr(file_path, ? + 1, 1) = '/'
            )
            OR (
                length(directory_path) > ?
                AND substr(directory_path, 1, ?) = ?
                AND substr(directory_path, ? + 1, 1) = '/'
            )
        ",
    )
    .bind(path)
    .bind(path)
    .bind(path_len)
    .bind(path_len)
    .bind(path)
    .bind(path_len)
    .bind(path_len)
    .bind(path_len)
    .bind(path)
    .bind(path_len)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    delete_file_chunk_vector_refs(transaction, decode_file_chunk_vector_refs(rows)?).await
}

async fn delete_all_file_chunk_vectors(transaction: &mut Transaction<'_, Sqlite>) -> Result<()> {
    let table_names = sqlx::query_scalar::<_, String>(
        "
        SELECT name
        FROM sqlite_master
        WHERE type = 'table' AND name LIKE 'indexed_file_chunk_vec_%'
        ",
    )
    .fetch_all(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    for table_name in table_names
        .into_iter()
        .filter(|name| is_file_chunk_vector_table_name(name))
    {
        let sql = format!("DELETE FROM {table_name}");
        sqlx::query(&sql)
            .execute(&mut **transaction)
            .await
            .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    }

    sqlx::query("DELETE FROM indexed_file_chunk_vector_refs")
        .execute(&mut **transaction)
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    Ok(())
}

async fn delete_file_chunk_vector_refs(
    transaction: &mut Transaction<'_, Sqlite>,
    refs: Vec<FileChunkVectorRef>,
) -> Result<()> {
    for reference in refs {
        let table_name = file_chunk_vector_table_name(reference.embedding_dim)?;
        let sql = format!("DELETE FROM {table_name} WHERE rowid = ?");
        sqlx::query(&sql)
            .bind(reference.vector_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| DbError::SyncFileChunkVectors { source })?;

        sqlx::query("DELETE FROM indexed_file_chunk_vector_refs WHERE vector_id = ?")
            .bind(reference.vector_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| DbError::SyncFileChunkVectors { source })?;
    }

    Ok(())
}

async fn indexed_file_for_vector_metadata(
    transaction: &mut Transaction<'_, Sqlite>,
    file_path: &str,
) -> Result<Option<IndexedFile>> {
    let row = sqlx::query(
        "
        SELECT
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
        FROM indexed_files
        WHERE path = ?
        ",
    )
    .bind(file_path)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    row.map(decode_indexed_file_row)
        .transpose()
        .map_err(|source| DbError::SyncFileChunkVectors { source })
}

async fn insert_current_file_chunk_vector(
    transaction: &mut Transaction<'_, Sqlite>,
    file: &IndexedFile,
    chunk: &IndexedFileChunk,
) -> Result<()> {
    insert_file_chunk_vector_ref(
        transaction,
        FileChunkVectorWrite {
            source: "current",
            history_id: 0,
            file_path: &chunk.file_path,
            directory_path: &chunk.directory_path,
            chunk_index: i64::from(chunk.chunk_index),
            embedding: &chunk.embedding,
            file_modified_unix_seconds: file.modified_unix_seconds,
            directory_modified_unix_seconds: file.modified_unix_seconds,
            indexed_unix_seconds: chunk.indexed_unix_seconds,
        },
    )
    .await
}

async fn insert_history_file_chunk_vector(
    transaction: &mut Transaction<'_, Sqlite>,
    history_id: i64,
    file: &IndexedFile,
    chunk: &IndexedFileChunk,
) -> Result<()> {
    insert_file_chunk_vector_ref(
        transaction,
        FileChunkVectorWrite {
            source: "history",
            history_id,
            file_path: &chunk.file_path,
            directory_path: &chunk.directory_path,
            chunk_index: i64::from(chunk.chunk_index),
            embedding: &chunk.embedding,
            file_modified_unix_seconds: file.modified_unix_seconds,
            directory_modified_unix_seconds: file.modified_unix_seconds,
            indexed_unix_seconds: chunk.indexed_unix_seconds,
        },
    )
    .await
}

async fn insert_file_chunk_vector_ref(
    transaction: &mut Transaction<'_, Sqlite>,
    row: FileChunkVectorWrite<'_>,
) -> Result<()> {
    if row.embedding.is_empty() {
        return Ok(());
    }

    let embedding_dim = i64::try_from(row.embedding.len())
        .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?;
    sqlx::query(
        "
        INSERT OR IGNORE INTO indexed_file_chunk_vector_refs (
            source,
            history_id,
            file_path,
            directory_path,
            chunk_index,
            embedding_dim,
            file_modified_unix_seconds,
            directory_modified_unix_seconds,
            indexed_unix_seconds
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(row.source)
    .bind(row.history_id)
    .bind(row.file_path)
    .bind(row.directory_path)
    .bind(row.chunk_index)
    .bind(embedding_dim)
    .bind(row.file_modified_unix_seconds)
    .bind(row.directory_modified_unix_seconds)
    .bind(row.indexed_unix_seconds)
    .execute(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    let vector_id: i64 = sqlx::query_scalar(
        "
        SELECT vector_id
        FROM indexed_file_chunk_vector_refs
        WHERE source = ?
            AND history_id = ?
            AND file_path = ?
            AND chunk_index = ?
        ",
    )
    .bind(row.source)
    .bind(row.history_id)
    .bind(row.file_path)
    .bind(row.chunk_index)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    insert_file_chunk_vector_row(
        transaction,
        vector_id,
        row.embedding,
        row.file_modified_unix_seconds,
    )
    .await
}

async fn insert_file_chunk_vector_row(
    transaction: &mut Transaction<'_, Sqlite>,
    vector_id: i64,
    embedding: &[f32],
    file_modified_unix_seconds: i64,
) -> Result<()> {
    let table_name = file_chunk_vector_table_name(embedding.len())?;
    let delete_sql = format!("DELETE FROM {table_name} WHERE rowid = ?");
    sqlx::query(&delete_sql)
        .bind(vector_id)
        .execute(&mut **transaction)
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    let insert_sql = format!(
        "
        INSERT INTO {table_name} (
            rowid,
            embedding,
            file_modified_unix_seconds
        ) VALUES (?, ?, ?)
        "
    );
    sqlx::query(&insert_sql)
        .bind(vector_id)
        .bind(encode_embedding(embedding))
        .bind(file_modified_unix_seconds)
        .execute(&mut **transaction)
        .await
        .map_err(|source| DbError::SyncFileChunkVectors { source })?;

    Ok(())
}

fn decode_file_chunk_vector_refs(rows: Vec<SqliteRow>) -> Result<Vec<FileChunkVectorRef>> {
    rows.into_iter()
        .map(|row| {
            let embedding_dim: i64 = row
                .try_get("embedding_dim")
                .map_err(|source| DbError::SyncFileChunkVectors { source })?;
            Ok(FileChunkVectorRef {
                vector_id: row
                    .try_get("vector_id")
                    .map_err(|source| DbError::SyncFileChunkVectors { source })?,
                embedding_dim: usize::try_from(embedding_dim)
                    .map_err(|_| DbError::InvalidEmbeddingDimension { dimension: 0 })?,
            })
        })
        .collect()
}

fn push_directory_term_filter(query: &mut QueryBuilder<'_, Sqlite>, terms: &[String]) {
    for (index, term) in terms.iter().enumerate() {
        if index > 0 {
            query.push(" OR ");
        }
        let pattern = format!("%{}%", escape_like_pattern(term));
        query.push("(");
        query.push("lower(name) LIKE ");
        query.push_bind(pattern.clone());
        query.push(" ESCAPE '\\' OR lower(path) LIKE ");
        query.push_bind(pattern);
        query.push(" ESCAPE '\\')");
    }
}

fn unique_directory_paths(directory_paths: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    directory_paths
        .iter()
        .filter(|path| seen.insert((*path).clone()))
        .cloned()
        .collect()
}

fn push_directory_tree_root_values(
    query: &mut QueryBuilder<'_, Sqlite>,
    directory_paths: &[String],
) {
    for (index, path) in directory_paths.iter().enumerate() {
        if index > 0 {
            query.push(", ");
        }
        let child_pattern = format!("{}/%", escape_like_pattern(path));
        query.push("(");
        query.push_bind(path.clone());
        query.push(", ");
        query.push_bind(child_pattern);
        query.push(")");
    }
}

fn push_directory_tree_exists_filter(query: &mut QueryBuilder<'_, Sqlite>, column: &'static str) {
    query.push("EXISTS (SELECT 1 FROM directory_tree_roots AS root WHERE ");
    query.push(column);
    query.push(" = root.path OR ");
    query.push(column);
    query.push(" LIKE root.child_pattern ESCAPE '\\')");
}

fn push_modified_time_filter(
    query: &mut QueryBuilder<'_, Sqlite>,
    column: &'static str,
    range: ModifiedTimeRange,
) {
    let mut has_previous_filter = false;
    if let Some(start) = range.start_unix_seconds {
        query.push(column);
        query.push(" >= ");
        query.push_bind(start);
        has_previous_filter = true;
    }
    if let Some(end) = range.end_unix_seconds {
        if has_previous_filter {
            query.push(" AND ");
        }
        query.push(column);
        query.push(" < ");
        query.push_bind(end);
        has_previous_filter = true;
    }
    if !has_previous_filter {
        query.push("1 = 1");
    }
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' | '%' | '_' => {
                escaped.push('\\');
                escaped.push(ch);
            }
            _ => escaped.push(ch),
        }
    }
    escaped
}

async fn prepare_legacy_schema(pool: &SqlitePool) -> Result<()> {
    let has_legacy_documents: Option<i64> = sqlx::query_scalar(
        "
        SELECT 1
        FROM sqlite_master
        WHERE type = 'table' AND name = 'indexed_documents'
        ",
    )
    .fetch_optional(pool)
    .await
    .map_err(|source| DbError::PrepareLegacyMigration { source })?;

    if has_legacy_documents.is_none() {
        return Ok(());
    }

    let rows = sqlx::query("PRAGMA table_info(indexed_documents)")
        .fetch_all(pool)
        .await
        .map_err(|source| DbError::PrepareLegacyMigration { source })?;
    let columns = rows
        .into_iter()
        .map(|row| row.try_get::<String, _>("name"))
        .collect::<std::result::Result<HashSet<_>, _>>()
        .map_err(|source| DbError::PrepareLegacyMigration { source })?;

    add_legacy_column_if_missing(
        pool,
        &columns,
        "name",
        "ALTER TABLE indexed_documents ADD COLUMN name TEXT NOT NULL DEFAULT ''",
    )
    .await?;
    add_legacy_column_if_missing(
        pool,
        &columns,
        "size_bytes",
        "ALTER TABLE indexed_documents ADD COLUMN size_bytes INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    add_legacy_column_if_missing(
        pool,
        &columns,
        "created_unix_seconds",
        "ALTER TABLE indexed_documents ADD COLUMN created_unix_seconds INTEGER",
    )
    .await?;
    add_legacy_column_if_missing(
        pool,
        &columns,
        "accessed_unix_seconds",
        "ALTER TABLE indexed_documents ADD COLUMN accessed_unix_seconds INTEGER",
    )
    .await?;
    add_legacy_column_if_missing(
        pool,
        &columns,
        "readonly",
        "ALTER TABLE indexed_documents ADD COLUMN readonly INTEGER NOT NULL DEFAULT 0",
    )
    .await?;

    Ok(())
}

async fn add_legacy_column_if_missing(
    pool: &SqlitePool,
    columns: &HashSet<String>,
    column: &str,
    sql: &str,
) -> Result<()> {
    if columns.contains(column) {
        return Ok(());
    }

    sqlx::query(sql)
        .execute(pool)
        .await
        .map_err(|source| DbError::PrepareLegacyMigration { source })?;
    Ok(())
}

async fn bump_index_revision(transaction: &mut Transaction<'_, Sqlite>) -> Result<()> {
    sqlx::query(
        "
        INSERT INTO index_metadata (key, value)
        VALUES ('revision', 1)
        ON CONFLICT(key) DO UPDATE SET value = value + 1
        ",
    )
    .execute(&mut **transaction)
    .await
    .map_err(|source| DbError::BumpIndexRevision { source })?;

    Ok(())
}

async fn insert_file_chunk_history(
    transaction: &mut Transaction<'_, Sqlite>,
    file: &IndexedFile,
    chunk: &IndexedFileChunk,
) -> Result<i64> {
    sqlx::query(
        "
        INSERT OR IGNORE INTO indexed_file_chunk_history (
            file_path,
            file_name,
            directory_path,
            chunk_index,
            content,
            embedding,
            embedding_dim,
            start_byte,
            end_byte,
            content_fingerprint,
            file_modified_unix_seconds,
            directory_modified_unix_seconds,
            indexed_unix_seconds
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ",
    )
    .bind(&chunk.file_path)
    .bind(&file.name)
    .bind(&chunk.directory_path)
    .bind(i64::from(chunk.chunk_index))
    .bind(&chunk.content)
    .bind(encode_embedding(&chunk.embedding))
    .bind(
        i64::try_from(chunk.embedding.len())
            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?,
    )
    .bind(
        i64::try_from(chunk.start_byte)
            .map_err(|source| DbError::MetadataSizeOverflow { source })?,
    )
    .bind(i64::try_from(chunk.end_byte).map_err(|source| DbError::MetadataSizeOverflow { source })?)
    .bind(&file.content_fingerprint)
    .bind(file.modified_unix_seconds)
    .bind(file.modified_unix_seconds)
    .bind(chunk.indexed_unix_seconds)
    .execute(&mut **transaction)
    .await
    .map_err(|source| DbError::InsertFileChunkHistory {
        path: chunk.file_path.clone(),
        chunk_index: chunk.chunk_index,
        source,
    })?;

    sqlx::query_scalar(
        "
        SELECT id
        FROM indexed_file_chunk_history
        WHERE file_path = ?
            AND content_fingerprint = ?
            AND chunk_index = ?
        ",
    )
    .bind(&chunk.file_path)
    .bind(&file.content_fingerprint)
    .bind(i64::from(chunk.chunk_index))
    .fetch_one(&mut **transaction)
    .await
    .map_err(|source| DbError::InsertFileChunkHistory {
        path: chunk.file_path.clone(),
        chunk_index: chunk.chunk_index,
        source,
    })
}

fn decode_classification_row(
    row: SqliteRow,
) -> std::result::Result<DirectoryClassification, sqlx::Error> {
    let confidence: f64 = row.try_get("confidence")?;
    Ok(DirectoryClassification {
        directory_path: row.try_get("directory_path")?,
        label: row.try_get("label")?,
        confidence: confidence as f32,
        detector: row.try_get("detector")?,
        evidence_path: row.try_get("evidence_path")?,
        evidence_summary: row.try_get("evidence_summary")?,
        detected_unix_seconds: row.try_get("detected_unix_seconds")?,
    })
}

fn decode_directory_type_count_row(
    row: SqliteRow,
) -> std::result::Result<DirectoryTypeCount, sqlx::Error> {
    let count: i64 = row.try_get("directory_count")?;
    Ok(DirectoryTypeCount {
        label: row.try_get("label")?,
        count: u64::try_from(count).map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
    })
}

fn decode_document_row(row: SqliteRow) -> std::result::Result<IndexedDocument, sqlx::Error> {
    let path: String = row.try_get("path")?;
    let kind: String = row.try_get("kind")?;
    let embedding: Vec<u8> = row.try_get("embedding")?;
    let size_bytes: i64 = row.try_get("size_bytes")?;
    Ok(IndexedDocument {
        path,
        name: row.try_get("name")?,
        kind: DocumentKind::from_db_value(&kind),
        parent_path: row.try_get("parent_path")?,
        searchable_text: row.try_get("searchable_text")?,
        embedding: decode_embedding(&embedding)
            .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        metadata_fingerprint: row.try_get("metadata_fingerprint")?,
        size_bytes: u64::try_from(size_bytes).map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        created_unix_seconds: row.try_get("created_unix_seconds")?,
        modified_unix_seconds: row.try_get("modified_unix_seconds")?,
        accessed_unix_seconds: row.try_get("accessed_unix_seconds")?,
        readonly: row.try_get("readonly")?,
        indexed_unix_seconds: row.try_get("indexed_unix_seconds")?,
    })
}

fn decode_directory_search_row(
    row: SqliteRow,
) -> std::result::Result<IndexedDocument, sqlx::Error> {
    let kind: String = row.try_get("kind")?;
    let size_bytes: i64 = row.try_get("size_bytes")?;
    Ok(IndexedDocument {
        path: row.try_get("path")?,
        name: row.try_get("name")?,
        kind: DocumentKind::from_db_value(&kind),
        parent_path: row.try_get("parent_path")?,
        searchable_text: String::new(),
        embedding: Vec::new(),
        metadata_fingerprint: String::new(),
        size_bytes: u64::try_from(size_bytes).map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        created_unix_seconds: row.try_get("created_unix_seconds")?,
        modified_unix_seconds: row.try_get("modified_unix_seconds")?,
        accessed_unix_seconds: row.try_get("accessed_unix_seconds")?,
        readonly: row.try_get("readonly")?,
        indexed_unix_seconds: row.try_get("indexed_unix_seconds")?,
    })
}

fn decode_indexed_file_row(row: SqliteRow) -> std::result::Result<IndexedFile, sqlx::Error> {
    let size_bytes: i64 = row.try_get("size_bytes")?;
    Ok(IndexedFile {
        path: row.try_get("path")?,
        directory_path: row.try_get("directory_path")?,
        name: row.try_get("name")?,
        extension: row.try_get("extension")?,
        size_bytes: u64::try_from(size_bytes).map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        created_unix_seconds: row.try_get("created_unix_seconds")?,
        modified_unix_seconds: row.try_get("modified_unix_seconds")?,
        accessed_unix_seconds: row.try_get("accessed_unix_seconds")?,
        readonly: row.try_get("readonly")?,
        content_fingerprint: row.try_get("content_fingerprint")?,
        indexed_unix_seconds: row.try_get("indexed_unix_seconds")?,
    })
}

fn decode_file_chunk_match_row(row: SqliteRow) -> std::result::Result<FileChunkMatch, sqlx::Error> {
    let embedding: Vec<u8> = row.try_get("embedding")?;
    Ok(FileChunkMatch {
        file_path: row.try_get("file_path")?,
        file_name: row.try_get("file_name")?,
        directory_path: row.try_get("directory_path")?,
        content: row.try_get("content")?,
        embedding: decode_embedding(&embedding)
            .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        is_current: row.try_get::<i64, _>("is_current")? != 0,
        file_modified_unix_seconds: row.try_get("file_modified_unix_seconds")?,
        directory_modified_unix_seconds: row.try_get("directory_modified_unix_seconds")?,
    })
}

fn decode_backfill_file_chunk_vector_row(
    row: SqliteRow,
) -> std::result::Result<BackfillFileChunkVector, sqlx::Error> {
    let source: String = row.try_get("source")?;
    let embedding: Vec<u8> = row.try_get("embedding")?;
    Ok(BackfillFileChunkVector {
        source: match source.as_str() {
            "history" => "history",
            _ => "current",
        },
        history_id: row.try_get("history_id")?,
        file_path: row.try_get("file_path")?,
        directory_path: row.try_get("directory_path")?,
        chunk_index: row.try_get("chunk_index")?,
        embedding: decode_embedding(&embedding)
            .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        file_modified_unix_seconds: row.try_get("file_modified_unix_seconds")?,
        directory_modified_unix_seconds: row.try_get("directory_modified_unix_seconds")?,
        indexed_unix_seconds: row.try_get("indexed_unix_seconds")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn upserts_and_reads_document() {
        let db = Database::open_in_memory().await.unwrap();
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

        db.upsert_document(&document).await.unwrap();

        assert_eq!(db.document_count().await.unwrap(), 1);
        assert_eq!(
            db.get_document("/tmp/project").await.unwrap(),
            Some(document)
        );
    }

    #[tokio::test]
    async fn registers_sqlite_vec_extension() {
        let db = Database::open_in_memory().await.unwrap();
        let version: String = sqlx::query_scalar("SELECT vec_version()")
            .fetch_one(&db.pool)
            .await
            .unwrap();

        assert!(version.starts_with("v"));
    }

    #[tokio::test]
    async fn nearest_file_chunk_matches_use_vector_index() {
        let db = Database::open_in_memory().await.unwrap();
        for path in ["/tmp/chrome-extension", "/tmp/notes"] {
            db.upsert_document(&IndexedDocument {
                path: path.to_string(),
                name: path.rsplit('/').next().unwrap().to_string(),
                kind: DocumentKind::Directory,
                parent_path: Some("/tmp".to_string()),
                searchable_text: path.to_string(),
                embedding: vec![0.0, 0.0, 0.0],
                metadata_fingerprint: format!("directory:{path}"),
                size_bytes: 0,
                created_unix_seconds: None,
                modified_unix_seconds: 10,
                accessed_unix_seconds: None,
                readonly: false,
                indexed_unix_seconds: 10,
            })
            .await
            .unwrap();
        }

        let chrome_file = IndexedFile {
            path: "/tmp/chrome-extension/README.md".to_string(),
            directory_path: "/tmp/chrome-extension".to_string(),
            name: "README.md".to_string(),
            extension: Some("md".to_string()),
            size_bytes: 20,
            created_unix_seconds: None,
            modified_unix_seconds: 20,
            accessed_unix_seconds: None,
            readonly: false,
            content_fingerprint: "chrome".to_string(),
            indexed_unix_seconds: 20,
        };
        let notes_file = IndexedFile {
            path: "/tmp/notes/README.md".to_string(),
            directory_path: "/tmp/notes".to_string(),
            name: "README.md".to_string(),
            extension: Some("md".to_string()),
            size_bytes: 20,
            created_unix_seconds: None,
            modified_unix_seconds: 20,
            accessed_unix_seconds: None,
            readonly: false,
            content_fingerprint: "notes".to_string(),
            indexed_unix_seconds: 20,
        };
        let chrome_chunk = IndexedFileChunk {
            file_path: chrome_file.path.clone(),
            directory_path: chrome_file.directory_path.clone(),
            chunk_index: 0,
            content: "chrome extension manifest".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
            start_byte: 0,
            end_byte: 20,
            indexed_unix_seconds: 20,
        };
        let notes_chunk = IndexedFileChunk {
            file_path: notes_file.path.clone(),
            directory_path: notes_file.directory_path.clone(),
            chunk_index: 0,
            content: "meeting notes agenda".to_string(),
            embedding: vec![0.0, 1.0, 0.0],
            start_byte: 0,
            end_byte: 20,
            indexed_unix_seconds: 20,
        };

        db.upsert_files_with_chunks(&[
            (&chrome_file, std::slice::from_ref(&chrome_chunk)),
            (&notes_file, std::slice::from_ref(&notes_chunk)),
        ])
        .await
        .unwrap();

        let matches = db
            .nearest_file_chunk_matches_with_modified_range(&[1.0, 0.0, 0.0], None, 1)
            .await
            .unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].directory_path, "/tmp/chrome-extension");
        assert_eq!(table_count(&db, "indexed_file_chunk_vector_refs").await, 4);
    }

    #[tokio::test]
    async fn indexed_mutations_bump_revision() {
        let db = Database::open_in_memory().await.unwrap();
        let start = db.current_revision().await.unwrap();

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
            readonly: false,
            indexed_unix_seconds: 34,
        })
        .await
        .unwrap();
        assert_eq!(db.current_revision().await.unwrap(), start + 1);

        db.reset().await.unwrap();
        assert_eq!(db.current_revision().await.unwrap(), start + 2);
    }

    #[tokio::test]
    async fn file_chunk_tree_query_handles_many_candidate_roots() {
        let db = Database::open_in_memory().await.unwrap();
        let directory_paths = (0..1100)
            .map(|index| format!("/tmp/project-{index}"))
            .collect::<Vec<_>>();

        let matches = db
            .file_chunk_matches_in_directory_trees(&directory_paths)
            .await
            .unwrap();

        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn migrates_v1_database_to_metadata_schema() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("cds.sqlite");
        let pool = open_file_pool(&path, true).await.unwrap();
        sqlx::query(
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
            )
            ",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("PRAGMA user_version = 1")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let db = Database::open(&path).await.unwrap();
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&db.pool)
            .await
            .unwrap();

        assert_eq!(version, 8);
        assert_eq!(db.current_revision().await.unwrap(), 0);
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
        .await
        .unwrap();

        let document = db.get_document("/tmp/project").await.unwrap().unwrap();
        assert_eq!(document.name, "project");
        assert_eq!(document.size_bytes, 4096);
        assert_eq!(document.created_unix_seconds, Some(10));
        assert_eq!(document.accessed_unix_seconds, Some(14));
        assert!(document.readonly);
    }

    #[tokio::test]
    async fn replaces_and_reads_directory_classifications() {
        let db = Database::open_in_memory().await.unwrap();
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
            .await
            .unwrap();
        assert_eq!(
            db.directory_classifications("/tmp/project").await.unwrap(),
            vec![classification]
        );

        db.replace_directory_classifications("/tmp/project", &[])
            .await
            .unwrap();
        assert!(
            db.directory_classifications("/tmp/project")
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn counts_distinct_directories_by_type() {
        let db = Database::open_in_memory().await.unwrap();
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
            .await
            .unwrap();
        db.replace_directory_classifications("/tmp/two", &[rust_two])
            .await
            .unwrap();
        db.replace_directory_classifications("/tmp/three", &[chrome])
            .await
            .unwrap();

        assert_eq!(
            db.directory_type_counts().await.unwrap(),
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

    #[tokio::test]
    async fn reset_clears_indexed_content() {
        let db = Database::open_in_memory().await.unwrap();
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
        .await
        .unwrap();
        let indexed_file = IndexedFile {
            path: "/tmp/project/README.md".to_string(),
            directory_path: "/tmp/project".to_string(),
            name: "README.md".to_string(),
            extension: Some("md".to_string()),
            size_bytes: 12,
            created_unix_seconds: Some(10),
            modified_unix_seconds: 12,
            accessed_unix_seconds: Some(14),
            readonly: false,
            content_fingerprint: "mtime:12:len:12:hash:abc".to_string(),
            indexed_unix_seconds: 34,
        };
        let indexed_chunk = IndexedFileChunk {
            file_path: indexed_file.path.clone(),
            directory_path: indexed_file.directory_path.clone(),
            chunk_index: 0,
            content: "project readme cargo".to_string(),
            embedding: vec![0.1, 0.2, 0.3],
            start_byte: 0,
            end_byte: 20,
            indexed_unix_seconds: 34,
        };
        db.upsert_files_with_chunks(&[(&indexed_file, &[indexed_chunk])])
            .await
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
        .await
        .unwrap();

        db.reset().await.unwrap();

        assert_eq!(db.document_count().await.unwrap(), 0);
        assert_eq!(table_count(&db, "indexed_files").await, 0);
        assert_eq!(table_count(&db, "indexed_file_chunks").await, 0);
        assert_eq!(table_count(&db, "indexed_file_chunk_history").await, 0);
        assert_eq!(table_count(&db, "indexed_file_chunk_vector_refs").await, 0);
        assert_eq!(table_count(&db, "directory_classifications").await, 0);
        assert!(db.directory_type_counts().await.unwrap().is_empty());
    }

    async fn table_count(db: &Database, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        sqlx::query_scalar(&sql).fetch_one(&db.pool).await.unwrap()
    }
}
