CREATE TABLE IF NOT EXISTS indexed_file_chunk_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT NOT NULL,
    file_name TEXT NOT NULL,
    directory_path TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    content TEXT NOT NULL,
    embedding BLOB NOT NULL,
    embedding_dim INTEGER NOT NULL,
    start_byte INTEGER NOT NULL,
    end_byte INTEGER NOT NULL,
    content_fingerprint TEXT NOT NULL,
    file_modified_unix_seconds INTEGER NOT NULL,
    directory_modified_unix_seconds INTEGER NOT NULL,
    indexed_unix_seconds INTEGER NOT NULL,
    UNIQUE (file_path, content_fingerprint, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_history_directory_path
    ON indexed_file_chunk_history(directory_path);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_history_file_path
    ON indexed_file_chunk_history(file_path);
