use super::{Embedder, Result};

#[derive(Debug, Clone)]
pub struct FakeEmbedder {
    dimensions: usize,
}

impl FakeEmbedder {
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

impl Default for FakeEmbedder {
    fn default() -> Self {
        Self::new(32)
    }
}

impl Embedder for FakeEmbedder {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let mut values = vec![0.0; self.dimensions];

        for token in text.split(|ch: char| !ch.is_alphanumeric()) {
            if token.is_empty() {
                continue;
            }

            let hash = stable_hash(token.as_bytes());
            let index = hash as usize % self.dimensions;
            values[index] += 1.0;
        }

        normalize(&mut values);
        Ok(values)
    }
}

fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in bytes {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash
}

fn normalize(values: &mut [f32]) {
    let magnitude = values.iter().map(|value| value * value).sum::<f32>().sqrt();
    if magnitude == 0.0 {
        return;
    }

    for value in values {
        *value /= magnitude;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_embeddings_are_deterministic() {
        let embedder = FakeEmbedder::new(8);
        assert_eq!(
            embedder.embed("Chrome Extension").unwrap(),
            embedder.embed("Chrome Extension").unwrap()
        );
    }

    #[test]
    fn fake_embeddings_use_requested_dimension() {
        let embedder = FakeEmbedder::new(12);
        assert_eq!(embedder.embed("hello").unwrap().len(), 12);
    }
}
