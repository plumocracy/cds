CREATE TABLE IF NOT EXISTS indexed_file_chunk_vector_refs (
    vector_id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL CHECK (source IN ('current', 'history')),
    history_id INTEGER NOT NULL DEFAULT 0,
    file_path TEXT NOT NULL,
    directory_path TEXT NOT NULL,
    chunk_index INTEGER NOT NULL,
    embedding_dim INTEGER NOT NULL,
    file_modified_unix_seconds INTEGER NOT NULL,
    directory_modified_unix_seconds INTEGER NOT NULL,
    indexed_unix_seconds INTEGER NOT NULL,
    UNIQUE (source, history_id, file_path, chunk_index)
);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_vector_refs_dim
    ON indexed_file_chunk_vector_refs(embedding_dim);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_vector_refs_file_path
    ON indexed_file_chunk_vector_refs(file_path);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_vector_refs_directory_path
    ON indexed_file_chunk_vector_refs(directory_path);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_vector_refs_modified_time
    ON indexed_file_chunk_vector_refs(file_modified_unix_seconds);
