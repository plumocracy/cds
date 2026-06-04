use thiserror::Error;

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("embedding model failed: {message}")]
    Model { message: String },
}
