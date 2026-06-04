#[derive(Debug, Clone, PartialEq)]
pub struct IndexedDocument {
    pub path: String,
    pub name: String,
    pub kind: DocumentKind,
    pub parent_path: Option<String>,
    pub searchable_text: String,
    pub embedding: Vec<f32>,
    pub metadata_fingerprint: String,
    pub size_bytes: u64,
    pub created_unix_seconds: Option<i64>,
    pub modified_unix_seconds: i64,
    pub accessed_unix_seconds: Option<i64>,
    pub readonly: bool,
    pub indexed_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub path: String,
    pub directory_path: String,
    pub name: String,
    pub extension: Option<String>,
    pub size_bytes: u64,
    pub created_unix_seconds: Option<i64>,
    pub modified_unix_seconds: i64,
    pub accessed_unix_seconds: Option<i64>,
    pub readonly: bool,
    pub content_fingerprint: String,
    pub indexed_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedFileChunk {
    pub file_path: String,
    pub directory_path: String,
    pub chunk_index: u32,
    pub content: String,
    pub embedding: Vec<f32>,
    pub start_byte: u64,
    pub end_byte: u64,
    pub indexed_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileChunkMatch {
    pub file_path: String,
    pub file_name: String,
    pub directory_path: String,
    pub content: String,
    pub embedding: Vec<f32>,
    pub is_current: bool,
    pub file_modified_unix_seconds: i64,
    pub directory_modified_unix_seconds: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModifiedTimeRange {
    pub start_unix_seconds: Option<i64>,
    pub end_unix_seconds: Option<i64>,
}

impl ModifiedTimeRange {
    pub fn contains(self, unix_seconds: i64) -> bool {
        if self
            .start_unix_seconds
            .is_some_and(|start| unix_seconds < start)
        {
            return false;
        }
        if self.end_unix_seconds.is_some_and(|end| unix_seconds >= end) {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirectoryClassification {
    pub directory_path: String,
    pub label: String,
    pub confidence: f32,
    pub detector: String,
    pub evidence_path: Option<String>,
    pub evidence_summary: String,
    pub detected_unix_seconds: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryTypeCount {
    pub label: String,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentKind {
    Directory,
    File,
}

impl DocumentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
        }
    }

    pub fn from_db_value(value: &str) -> Self {
        match value {
            "file" => Self::File,
            _ => Self::Directory,
        }
    }
}
