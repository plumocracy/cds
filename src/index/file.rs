use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Settings;
use crate::db::{IndexedFile, IndexedFileChunk};
use crate::index::{IndexError, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedFileData {
    pub file: IndexedFile,
    pub chunks: Vec<IndexedFileChunk>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedIndexedFileData {
    pub file: IndexedFile,
    pub chunks: Vec<PreparedFileChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFileChunk {
    pub file_path: String,
    pub directory_path: String,
    pub chunk_index: u32,
    pub content: String,
    pub start_byte: u64,
    pub end_byte: u64,
    pub indexed_unix_seconds: i64,
}

impl PreparedIndexedFileData {
    pub fn into_indexed(self, embeddings: &mut impl Iterator<Item = Vec<f32>>) -> IndexedFileData {
        let chunks = self
            .chunks
            .into_iter()
            .map(|chunk| {
                chunk.into_indexed(embeddings.next().expect("embedding count is validated"))
            })
            .collect();

        IndexedFileData {
            file: self.file,
            chunks,
        }
    }
}

impl PreparedFileChunk {
    fn into_indexed(self, embedding: Vec<f32>) -> IndexedFileChunk {
        IndexedFileChunk {
            file_path: self.file_path,
            directory_path: self.directory_path,
            chunk_index: self.chunk_index,
            content: self.content,
            embedding,
            start_byte: self.start_byte,
            end_byte: self.end_byte,
            indexed_unix_seconds: self.indexed_unix_seconds,
        }
    }
}

pub fn prepare_text_file(
    path: &Path,
    directory: &Path,
    settings: &Settings,
) -> Result<Option<PreparedIndexedFileData>> {
    let metadata = fs::metadata(path).map_err(|source| IndexError::StatFile {
        path: path.to_path_buf(),
        source,
    })?;

    if metadata.len() > settings.index.max_file_bytes {
        return Ok(None);
    }

    let name = path
        .file_name()
        .unwrap_or_else(|| OsStr::new(""))
        .to_string_lossy()
        .into_owned();
    if !settings.index.is_high_signal_file_name(&name) {
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
        chunks.push(PreparedFileChunk {
            file_path: file_path.clone(),
            directory_path: directory_path.clone(),
            chunk_index: u32::try_from(chunk_index).unwrap_or(u32::MAX),
            content: chunk.text.to_string(),
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            indexed_unix_seconds,
        });
    }

    Ok(Some(PreparedIndexedFileData { file, chunks }))
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

    #[test]
    fn skips_files_outside_high_signal_allowlist() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("notes.txt");
        fs::write(&path, "personal notes").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default()).unwrap();

        assert_eq!(prepared, None);
    }

    #[test]
    fn skips_svg_assets_even_though_they_are_text() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("logo.svg");
        fs::write(&path, "<svg><title>Logo</title></svg>").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default()).unwrap();

        assert_eq!(prepared, None);
    }

    #[test]
    fn prepares_high_signal_text_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("README.md");
        fs::write(&path, "high signal project summary").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default())
            .unwrap()
            .expect("README.md is indexed");

        assert_eq!(prepared.file.name, "README.md");
        assert_eq!(prepared.chunks.len(), 1);
    }

    #[test]
    fn prepares_project_descriptor_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("Cargo.toml");
        fs::write(&path, "[package]\nname = \"cds\"").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default())
            .unwrap()
            .expect("Cargo.toml is indexed");

        assert_eq!(prepared.file.name, "Cargo.toml");
        assert_eq!(prepared.chunks.len(), 1);
    }

    #[test]
    fn prepares_user_configured_high_signal_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ARCHITECTURE.md");
        fs::write(&path, "system architecture notes").unwrap();
        let settings = Settings {
            index: crate::config::IndexSettings {
                high_signal_files: vec!["ARCHITECTURE.md".to_string()],
                ..crate::config::IndexSettings::default()
            },
            ..Settings::default()
        };

        let prepared = prepare_text_file(&path, temp.path(), &settings)
            .unwrap()
            .expect("ARCHITECTURE.md is allowed by config");

        assert_eq!(prepared.file.name, "ARCHITECTURE.md");
        assert_eq!(prepared.chunks.len(), 1);
    }

    #[test]
    fn skips_source_code_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("main.rs");
        fs::write(&path, "fn main() { println!(\"hello\"); }").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default()).unwrap();

        assert_eq!(prepared, None);
    }

    #[test]
    fn skips_code_like_config_files() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("vite.config.ts");
        fs::write(&path, "export default { plugins: [] }").unwrap();

        let prepared = prepare_text_file(&path, temp.path(), &Settings::default()).unwrap();

        assert_eq!(prepared, None);
    }

    #[test]
    fn skips_generic_markdown_and_sql_files() {
        let temp = tempfile::tempdir().unwrap();
        let notes = temp.path().join("ARCHITECTURE.md");
        let schema = temp.path().join("schema.sql");
        fs::write(&notes, "internal implementation notes").unwrap();
        fs::write(&schema, "CREATE TABLE users (id INTEGER PRIMARY KEY);").unwrap();

        assert_eq!(
            prepare_text_file(&notes, temp.path(), &Settings::default()).unwrap(),
            None
        );
        assert_eq!(
            prepare_text_file(&schema, temp.path(), &Settings::default()).unwrap(),
            None
        );
    }
}
