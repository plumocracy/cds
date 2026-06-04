CREATE TABLE IF NOT EXISTS index_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value INTEGER NOT NULL
);

INSERT INTO index_metadata (key, value)
VALUES ('revision', 0)
ON CONFLICT(key) DO NOTHING;
