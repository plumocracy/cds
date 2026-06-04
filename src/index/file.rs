use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Settings;
use crate::db::{IndexedFile, IndexedFileChunk};
use crate::embed::Embedder;
use crate::index::{IndexError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedFileData {
    pub file: IndexedFile,
    pub chunks: Vec<IndexedFileChunk>,
}

pub fn index_text_file<E>(
    path: &Path,
    directory: &Path,
    settings: &Settings,
    embedder: &E,
) -> Result<Option<IndexedFileData>>
where
    E: Embedder,
{
    let metadata = fs::metadata(path).map_err(|source| IndexError::StatFile {
        path: path.to_path_buf(),
        source,
    })?;

    if metadata.len() > settings.index.max_file_bytes {
        return Ok(None);
    }

    let bytes = fs::read(path).map_err(|source| IndexError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.contains(&0) {
        return Ok(None);
    }

    let content = String::from_utf8_lossy(&bytes).into_owned();
    let normalized = normalize_whitespace(&content);
    if normalized.is_empty() {
        return Ok(None);
    }

    let indexed_unix_seconds = unix_seconds(SystemTime::now());
    let modified_unix_seconds = unix_seconds(metadata.modified().unwrap_or(UNIX_EPOCH));
    let file_path = path_to_string(path);
    let directory_path = path_to_string(directory);
    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new(""))
        .to_string_lossy()
        .into_owned();

    let file = IndexedFile {
        path: file_path.clone(),
        directory_path: directory_path.clone(),
        name,
        extension: path
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned()),
        size_bytes: metadata.len(),
        created_unix_seconds: metadata.created().ok().map(unix_seconds),
        modified_unix_seconds,
        accessed_unix_seconds: metadata.accessed().ok().map(unix_seconds),
        readonly: metadata.permissions().readonly(),
        content_fingerprint: format!(
            "mtime:{modified_unix_seconds}:len:{}:hash:{:016x}",
            metadata.len(),
            fnv1a64(&bytes),
        ),
        indexed_unix_seconds,
    };

    let mut chunks = Vec::new();
    for (chunk_index, chunk) in chunk_text(&normalized, settings.index.max_chunk_bytes)
        .into_iter()
        .enumerate()
    {
        let embedding = embedder
            .embed(chunk.text)
            .map_err(|source| IndexError::EmbedSummary {
                path: path.to_path_buf(),
                source,
            })?;

        chunks.push(IndexedFileChunk {
            file_path: file_path.clone(),
            directory_path: directory_path.clone(),
            chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
            content: chunk.text.to_string(),
            embedding,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            indexed_unix_seconds,
        });
    }

    Ok(Some(IndexedFileData { file, chunks }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextChunk<'a> {
    text: &'a str,
    start_byte: u64,
    end_byte: u64,
}

fn chunk_text(text: &str, max_chunk_bytes: usize) -> Vec<TextChunk<'_>> {
    let max_chunk_bytes = max_chunk_bytes.max(1);
    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let mut end = (start + max_chunk_bytes).min(text.len());
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }

        if end == start {
            end = text[start..]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| start + offset)
                .unwrap_or(text.len());
        }

        chunks.push(TextChunk {
            text: &text[start..end],
            start_byte: u64::try_from(start).unwrap_or(u64::MAX),
            end_byte: u64::try_from(end).unwrap_or(u64::MAX),
        });
        start = end;
    }

    chunks
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_text_on_char_boundaries() {
        let chunks = chunk_text("abc def ghi", 5);
        assert_eq!(chunks[0].text, "abc d");
        assert_eq!(chunks[1].text, "ef gh");
        assert_eq!(chunks[2].text, "i");
    }
}
