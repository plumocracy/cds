use super::DbError;

pub fn encode_embedding(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}

pub fn decode_embedding(bytes: &[u8]) -> Result<Vec<f32>, DbError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(DbError::InvalidEmbeddingBlobLength { len: bytes.len() });
    }

    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_round_trips() {
        let values = vec![0.25, -0.5, 2.0];
        assert_eq!(
            decode_embedding(&encode_embedding(&values)).unwrap(),
            values
        );
    }
}
