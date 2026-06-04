mod bge;
mod error;
mod fake;

pub use bge::{BGE_SMALL_EN_V15_DIMENSIONS, BgeSmallEmbedder};
pub use error::EmbedError;
pub use fake::FakeEmbedder;

pub type Result<T> = std::result::Result<T, EmbedError>;

pub trait Embedder {
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Vec<f32>>;

    fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }

    fn embed_documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|text| self.embed_document(text)).collect()
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text)
    }
}
