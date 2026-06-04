use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Settings;
use crate::db::{DocumentKind, IndexedDocument};
use crate::index::{IndexError, Result};

pub fn summarize_directory(directory: &Path, settings: &Settings) -> Result<IndexedDocument> {
    let metadata = fs::metadata(directory).map_err(|source| IndexError::ReadMetadata {
        path: directory.to_path_buf(),
        source,
    })?;
    let name = directory
        .file_name()
        .unwrap_or_else(|| OsStr::new(""))
        .to_string_lossy()
        .into_owned();
    let size_bytes = metadata.len();
    let created_unix_seconds = metadata.created().ok().map(unix_seconds);
    let modified_unix_seconds = unix_seconds(metadata.modified().unwrap_or(UNIX_EPOCH));
    let accessed_unix_seconds = metadata.accessed().ok().map(unix_seconds);
    let readonly = metadata.permissions().readonly();
    let indexed_unix_seconds = unix_seconds(SystemTime::now());
    let searchable_text = searchable_text(
        directory,
        &name,
        &DirectoryMetadata {
            size_bytes,
            created_unix_seconds,
            modified_unix_seconds,
            accessed_unix_seconds,
            readonly,
        },
        settings,
    )?;
    Ok(IndexedDocument {
        path: path_to_string(directory),
        name,
        kind: DocumentKind::Directory,
        parent_path: directory.parent().map(path_to_string),
        searchable_text,
        embedding: Vec::new(),
        metadata_fingerprint: format!(
            "mtime:{modified_unix_seconds}:ctime:{created}:atime:{accessed}:len:{size_bytes}:readonly:{readonly}",
            created = created_unix_seconds.unwrap_or(0),
            accessed = accessed_unix_seconds.unwrap_or(0),
        ),
        size_bytes,
        created_unix_seconds,
        modified_unix_seconds,
        accessed_unix_seconds,
        readonly,
        indexed_unix_seconds,
    })
}

#[derive(Debug, Clone, Copy)]
struct DirectoryMetadata {
    size_bytes: u64,
    created_unix_seconds: Option<i64>,
    modified_unix_seconds: i64,
    accessed_unix_seconds: Option<i64>,
    readonly: bool,
}

fn searchable_text(
    directory: &Path,
    name: &str,
    metadata: &DirectoryMetadata,
    settings: &Settings,
) -> Result<String> {
    let mut lines = vec![
        format!("directory: {}", path_to_string(directory)),
        format!("name: {name}"),
        "type: directory".to_string(),
        format!("size bytes: {}", metadata.size_bytes),
        format!(
            "created unix seconds: {}",
            metadata
                .created_unix_seconds
                .map(|seconds| seconds.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!("modified unix seconds: {}", metadata.modified_unix_seconds),
        format!(
            "accessed unix seconds: {}",
            metadata
                .accessed_unix_seconds
                .map(|seconds| seconds.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        ),
        format!("readonly: {}", metadata.readonly),
    ];

    let mut child_dirs = Vec::new();
    let mut child_files = Vec::new();
    let mut excerpts = Vec::new();

    let entries = fs::read_dir(directory).map_err(|source| IndexError::ReadDirectory {
        path: directory.to_path_buf(),
        source,
    })?;

    for entry in entries.take(settings.index.max_entries_per_directory) {
        let entry = entry.map_err(|source| IndexError::ReadDirectoryEntry {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();

        let file_type = entry
            .file_type()
            .map_err(|source| IndexError::ReadFileType {
                path: path.clone(),
                source,
            })?;

        if file_type.is_dir() {
            if settings.index.is_excluded_directory_name(&name) {
                continue;
            }

            child_dirs.push(name);
        } else if file_type.is_file() {
            if settings.index.is_excluded_name(&name) {
                continue;
            }

            child_files.push(name.clone());
            if is_high_signal_file(&name)
                && let Some(excerpt) = read_text_excerpt(
                    &path,
                    settings.index.max_file_bytes,
                    settings.index.max_excerpt_bytes,
                )?
            {
                excerpts.push(format!("file {name}: {excerpt}"));
            }
        }
    }

    child_dirs.sort();
    child_files.sort();
    excerpts.sort();

    if !child_dirs.is_empty() {
        lines.push(format!("child directories: {}", child_dirs.join(", ")));
    }

    if !child_files.is_empty() {
        lines.push(format!("child files: {}", child_files.join(", ")));
    }

    lines.extend(excerpts);
    Ok(lines.join("\n"))
}

fn is_high_signal_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "cargo.toml"
        || lower == "package.json"
        || lower == "pyproject.toml"
        || lower == "go.mod"
        || lower == "gemfile"
        || lower.starts_with("readme")
}

fn read_text_excerpt(
    path: &Path,
    max_file_bytes: u64,
    max_excerpt_bytes: usize,
) -> Result<Option<String>> {
    let metadata = fs::metadata(path).map_err(|source| IndexError::StatFile {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > max_file_bytes {
        return Ok(None);
    }

    let bytes = fs::read(path).map_err(|source| IndexError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.contains(&0) {
        return Ok(None);
    }

    let mut excerpt = String::from_utf8_lossy(&bytes).into_owned();
    excerpt = excerpt.chars().take(max_excerpt_bytes).collect();
    Ok(Some(normalize_whitespace(&excerpt)))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directory_summary_includes_names_and_high_signal_excerpt() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        fs::create_dir(temp.path().join(".vscode")).unwrap();
        fs::write(temp.path().join("README.md"), "Chrome extension workspace").unwrap();
        fs::write(temp.path().join(".env"), "SECRET=true").unwrap();
        fs::write(
            temp.path().join("logo.svg"),
            "<svg><title>Logo</title></svg>",
        )
        .unwrap();

        let settings = Settings::default();
        let document = summarize_directory(temp.path(), &settings).unwrap();

        assert_eq!(
            document.name,
            temp.path()
                .file_name()
                .unwrap_or_else(|| OsStr::new(""))
                .to_string_lossy()
        );
        assert_eq!(document.kind, DocumentKind::Directory);
        assert!(document.embedding.is_empty());
        assert!(document.size_bytes > 0);
        assert!(document.modified_unix_seconds > 0);
        assert!(document.searchable_text.contains("child directories: src"));
        assert!(!document.searchable_text.contains(".vscode"));
        assert!(!document.searchable_text.contains("logo.svg"));
        assert!(document.searchable_text.contains("README.md"));
        assert!(document.searchable_text.contains("type: directory"));
        assert!(document.searchable_text.contains("size bytes:"));
        assert!(document.searchable_text.contains("modified unix seconds:"));
        assert!(
            document
                .searchable_text
                .contains("Chrome extension workspace")
        );
        assert!(!document.searchable_text.contains("SECRET=true"));
    }
}
