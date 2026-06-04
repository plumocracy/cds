CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_history_modified_time
    ON indexed_file_chunk_history(file_modified_unix_seconds);

CREATE INDEX IF NOT EXISTS idx_indexed_file_chunk_history_directory_modified_time
    ON indexed_file_chunk_history(directory_path, file_modified_unix_seconds);
