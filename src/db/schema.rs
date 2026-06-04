use rusqlite::Connection;

use super::DbError;

const CURRENT_SCHEMA_VERSION: i64 = 4;

pub fn migrate(connection: &Connection) -> Result<(), DbError> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| DbError::ReadSchemaVersion { source })?;

    if version < 1 {
        migrate_to_v1(connection)?;
    }

    if version < 2 {
        migrate_to_v2(connection)?;
    }

    if version < 3 {
        migrate_to_v3(connection)?;
    }

    if version < 4 {
        migrate_to_v4(connection)?;
    }

    connection
        .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION)
        .map_err(|source| DbError::UpdateSchemaVersion { source })?;

    Ok(())
}

fn migrate_to_v1(connection: &Connection) -> Result<(), DbError> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS indexed_documents (
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

            CREATE INDEX IF NOT EXISTS idx_indexed_documents_kind
                ON indexed_documents(kind);

            CREATE INDEX IF NOT EXISTS idx_indexed_documents_parent_path
                ON indexed_documents(parent_path);

            CREATE INDEX IF NOT EXISTS idx_indexed_documents_indexed_time
                ON indexed_documents(indexed_unix_seconds);
            ",
        )
        .map_err(|source| DbError::CreateSchemaV1 { source })
}

fn migrate_to_v2(connection: &Connection) -> Result<(), DbError> {
    connection
        .execute_batch(
            "
            ALTER TABLE indexed_documents
                ADD COLUMN name TEXT NOT NULL DEFAULT '';

            ALTER TABLE indexed_documents
                ADD COLUMN size_bytes INTEGER NOT NULL DEFAULT 0;

            ALTER TABLE indexed_documents
                ADD COLUMN created_unix_seconds INTEGER;

            ALTER TABLE indexed_documents
                ADD COLUMN accessed_unix_seconds INTEGER;

            ALTER TABLE indexed_documents
                ADD COLUMN readonly INTEGER NOT NULL DEFAULT 0;

            CREATE INDEX IF NOT EXISTS idx_indexed_documents_name
                ON indexed_documents(name);
            ",
        )
        .map_err(|source| DbError::CreateSchemaV2 { source })
}

fn migrate_to_v3(connection: &Connection) -> Result<(), DbError> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS indexed_files (
                path TEXT PRIMARY KEY NOT NULL,
                directory_path TEXT NOT NULL,
                name TEXT NOT NULL,
                extension TEXT,
                size_bytes INTEGER NOT NULL,
                created_unix_seconds INTEGER,
                modified_unix_seconds INTEGER NOT NULL,
                accessed_unix_seconds INTEGER,
                readonly INTEGER NOT NULL,
                content_fingerprint TEXT NOT NULL,
                indexed_unix_seconds INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_indexed_files_directory_path
                ON indexed_files(directory_path);

            CREATE INDEX IF NOT EXISTS idx_indexed_files_modified_time
                ON indexed_files(modified_unix_seconds);

            CREATE TABLE IF NOT EXISTS indexed_file_chunks (
                file_path TEXT NOT NULL,
                directory_path TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                content TEXT NOT NULL,
                embedding BLOB NOT NULL,
                embedding_dim INTEGER NOT NULL,
                start_byte INTEGER NOT NULL,
                end_byte INTEGER NOT NULL,
                indexed_unix_seconds INTEGER NOT NULL,
                PRIMARY KEY (file_path, chunk_index)
            );

            CREATE INDEX IF NOT EXISTS idx_indexed_file_chunks_directory_path
                ON indexed_file_chunks(directory_path);
            ",
        )
        .map_err(|source| DbError::CreateSchemaV3 { source })
}

fn migrate_to_v4(connection: &Connection) -> Result<(), DbError> {
    connection
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS directory_classifications (
                directory_path TEXT NOT NULL,
                label TEXT NOT NULL,
                confidence REAL NOT NULL,
                detector TEXT NOT NULL,
                evidence_path TEXT,
                evidence_summary TEXT NOT NULL,
                detected_unix_seconds INTEGER NOT NULL,
                PRIMARY KEY (directory_path, label, detector)
            );

            CREATE INDEX IF NOT EXISTS idx_directory_classifications_label
                ON directory_classifications(label);

            CREATE INDEX IF NOT EXISTS idx_directory_classifications_directory_path
                ON directory_classifications(directory_path);
            ",
        )
        .map_err(|source| DbError::CreateSchemaV4 { source })
}
