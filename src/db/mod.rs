mod document;
mod error;
mod vector;

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};

pub use document::{
    DirectoryClassification, DirectoryTypeCount, DocumentKind, FileChunkMatch, IndexedDocument,
    IndexedFile, IndexedFileChunk,
};
pub use error::DbError;
pub use vector::{decode_embedding, encode_embedding};

pub type Result<T> = std::result::Result<T, DbError>;

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

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
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|source| DbError::OpenInMemory { source })?;
        migrate_pool(&pool).await?;
        Ok(Self { pool })
    }

    pub async fn upsert_document(&self, document: &IndexedDocument) -> Result<()> {
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
        .execute(&self.pool)
        .await
        .map_err(|source| DbError::UpsertDocument {
            path: document.path.clone(),
            source,
        })?;

        Ok(())
    }

    pub async fn upsert_file(&self, file: &IndexedFile) -> Result<()> {
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
        .execute(&self.pool)
        .await
        .map_err(|source| DbError::UpsertFile {
            path: file.path.clone(),
            source,
        })?;

        Ok(())
    }

    pub async fn replace_file_chunks(
        &self,
        file_path: &str,
        chunks: &[IndexedFileChunk],
    ) -> Result<()> {
        sqlx::query("DELETE FROM indexed_file_chunks WHERE file_path = ?")
            .bind(file_path)
            .execute(&self.pool)
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
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::InsertFileChunk {
                path: chunk.file_path.clone(),
                chunk_index: chunk.chunk_index,
                source,
            })?;
        }

        Ok(())
    }

    pub async fn upsert_files_with_chunks(
        &self,
        files: &[(&IndexedFile, &[IndexedFileChunk])],
    ) -> Result<()> {
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
            }
        }

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
        sqlx::query("DELETE FROM directory_classifications WHERE directory_path = ?")
            .bind(directory_path)
            .execute(&self.pool)
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
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::InsertDirectoryClassification {
                path: classification.directory_path.clone(),
                label: classification.label.clone(),
                source,
            })?;
        }

        Ok(())
    }

    pub async fn delete_path_tree(&self, path: &str) -> Result<()> {
        let path_len = i64::try_from(path.len())
            .map_err(|source| DbError::EmbeddingDimensionOverflow { source })?;

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
        .execute(&self.pool)
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
        .execute(&self.pool)
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
        .execute(&self.pool)
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
        .execute(&self.pool)
        .await
        .map_err(|source| DbError::DeletePathTree {
            path: path.to_string(),
            source,
        })?;

        Ok(())
    }

    pub async fn reset(&self) -> Result<()> {
        sqlx::query("DELETE FROM directory_classifications")
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_file_chunks")
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_files")
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::ResetDatabase { source })?;
        sqlx::query("DELETE FROM indexed_documents")
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

    pub async fn file_chunk_matches(&self) -> Result<Vec<FileChunkMatch>> {
        let rows = sqlx::query(
            "
            SELECT
                chunk.file_path,
                file.name AS file_name,
                chunk.directory_path,
                chunk.content,
                chunk.embedding,
                file.modified_unix_seconds AS file_modified_unix_seconds,
                directory.modified_unix_seconds AS directory_modified_unix_seconds
            FROM indexed_file_chunks AS chunk
            INNER JOIN indexed_files AS file
                ON file.path = chunk.file_path
            INNER JOIN indexed_documents AS directory
                ON directory.path = chunk.directory_path
            WHERE directory.kind = 'directory'
            ",
        )
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
    sqlx::query("PRAGMA user_version = 4")
        .execute(pool)
        .await
        .map_err(|source| DbError::PrepareLegacyMigration { source })?;
    Ok(())
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

fn decode_file_chunk_match_row(row: SqliteRow) -> std::result::Result<FileChunkMatch, sqlx::Error> {
    let embedding: Vec<u8> = row.try_get("embedding")?;
    Ok(FileChunkMatch {
        file_path: row.try_get("file_path")?,
        file_name: row.try_get("file_name")?,
        directory_path: row.try_get("directory_path")?,
        content: row.try_get("content")?,
        embedding: decode_embedding(&embedding)
            .map_err(|err| sqlx::Error::Decode(Box::new(err)))?,
        file_modified_unix_seconds: row.try_get("file_modified_unix_seconds")?,
        directory_modified_unix_seconds: row.try_get("directory_modified_unix_seconds")?,
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
        assert!(db.directory_type_counts().await.unwrap().is_empty());
    }
}
