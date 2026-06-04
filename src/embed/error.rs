use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("failed to create embedding model cache directory {path}")]
    CreateCacheDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("embedding model failed: {message}")]
    Model { message: String },

    #[error("embedding model lock is poisoned")]
    Lock,
}
