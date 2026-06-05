CREATE TABLE IF NOT EXISTS directory_visits (
    path TEXT PRIMARY KEY NOT NULL,
    score REAL NOT NULL,
    created_unix_seconds INTEGER NOT NULL,
    last_accessed_unix_seconds INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_directory_visits_last_accessed
    ON directory_visits(last_accessed_unix_seconds);
