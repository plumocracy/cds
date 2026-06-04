use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

use super::{EmbedError, Embedder, Result};

pub const BGE_SMALL_EN_V15_DIMENSIONS: usize = 384;

pub struct BgeSmallEmbedder {
    model: Mutex<TextEmbedding>,
}

impl BgeSmallEmbedder {
    pub fn new(cache_dir: impl AsRef<Path>) -> Result<Self> {
        let cache_dir = model_cache_dir(cache_dir.as_ref());
        fs::create_dir_all(&cache_dir).map_err(|source| EmbedError::CreateCacheDir {
            path: cache_dir.clone(),
            source,
        })?;

        let options = TextInitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false)
            .with_intra_threads(2);
        let model = TextEmbedding::try_new(options).map_err(|source| EmbedError::Model {
            message: source.to_string(),
        })?;

        Ok(Self {
            model: Mutex::new(model),
        })
    }

    fn embed_prefixed(&self, prefix: &str, text: &str) -> Result<Vec<f32>> {
        let input = format!("{prefix}: {text}");
        let mut model = self.model.lock().map_err(|_| EmbedError::Lock)?;
        let mut embeddings = model
            .embed([input], None)
            .map_err(|source| EmbedError::Model {
                message: source.to_string(),
            })?;

        embeddings.pop().ok_or_else(|| EmbedError::Model {
            message: "embedding model returned no vectors".to_string(),
        })
    }
}

impl Embedder for BgeSmallEmbedder {
    fn dimensions(&self) -> usize {
        BGE_SMALL_EN_V15_DIMENSIONS
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_document(text)
    }

    fn embed_document(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_prefixed("passage", text)
    }

    fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_prefixed("query", text)
    }
}

fn model_cache_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join("models")
}
