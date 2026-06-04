mod error;
mod fake;

pub use error::EmbedError;
pub use fake::FakeEmbedder;

pub type Result<T> = std::result::Result<T, EmbedError>;

pub trait Embedder {
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;
}
