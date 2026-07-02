use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{ConfigError, paths::expand_tilde};

pub type Result<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub index: IndexSettings,
    #[serde(default)]
    pub detectors: Vec<DirectoryTypeDefinition>,
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).map_err(|source| ConfigError::ReadConfig {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| ConfigError::ParseConfig {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }

        let settings = Self::default();
        settings.write(path)?;
        Ok(settings)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let text = serde_json::to_string_pretty(self)
            .map_err(|source| ConfigError::SerializeDefault { source })?;
        fs::write(path, text).map_err(|source| ConfigError::WriteConfig {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn expanded_roots(&self) -> Result<Vec<PathBuf>> {
        self.index
            .roots
            .iter()
            .map(|root| expand_tilde(root))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexSettings {
    pub roots: Vec<String>,
    pub exclude: Vec<String>,
    #[serde(default = "default_high_signal_files")]
    pub high_signal_files: Vec<String>,
    pub max_file_bytes: u64,
    pub max_excerpt_bytes: usize,
    pub max_entries_per_directory: usize,
    #[serde(default = "default_max_depth_per_top_level_directory")]
    pub max_depth_per_top_level_directory: usize,
    #[serde(default = "default_max_chunk_bytes")]
    pub max_chunk_bytes: usize,
    #[serde(default = "default_generic_terms")]
    pub generic_terms: Vec<String>,
}

impl IndexSettings {
    pub fn is_excluded_name(&self, name: &str) -> bool {
        self.exclude
            .iter()
            .any(|excluded| matches_name_pattern(excluded, name))
            || is_low_signal_name(name)
            || is_secret_name(name)
    }

    pub fn is_high_signal_file_name(&self, name: &str) -> bool {
        self.high_signal_files
            .iter()
            .any(|included| matches_name_pattern(included, name))
    }

    pub fn is_excluded_directory_component(&self, name: &str) -> bool {
        is_builtin_excluded_directory_name(name)
            || self
                .exclude
                .iter()
                .any(|excluded| matches_name_pattern(excluded, name))
            || is_low_signal_name(name)
            || is_secret_name(name)
    }

    pub fn is_excluded_directory_name(&self, name: &str) -> bool {
        is_hidden_name(name) || self.is_excluded_directory_component(name)
    }
}

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            roots: vec!["~/Projects".to_string()],
            exclude: vec![
                ".git".to_string(),
                "Applications".to_string(),
                "Library".to_string(),
                "Network".to_string(),
                "System".to_string(),
                "Volumes".to_string(),
                "cores".to_string(),
                "private".to_string(),
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string(),
                "build".to_string(),
                ".next".to_string(),
                ".cache".to_string(),
                ".venv".to_string(),
                "venv".to_string(),
                "vendor".to_string(),
                "*.xcassets".to_string(),
                "*.imageset".to_string(),
                "*.appiconset".to_string(),
                "*.colorset".to_string(),
            ],
            high_signal_files: default_high_signal_files(),
            max_file_bytes: 65_536,
            max_excerpt_bytes: 4_096,
            max_entries_per_directory: 80,
            max_depth_per_top_level_directory: default_max_depth_per_top_level_directory(),
            max_chunk_bytes: default_max_chunk_bytes(),
            generic_terms: default_generic_terms(),
        }
    }
}

pub fn default_high_signal_files() -> Vec<String> {
    [
        "README*",
        "Cargo.toml",
        "package.json",
        "manifest.json",
        "pyproject.toml",
        "requirements.txt",
        "go.mod",
        "Gemfile",
        "Dockerfile",
        "docker-compose.yml",
        "compose.yml",
        "compose.yaml",
        "tsconfig.json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

const fn default_max_depth_per_top_level_directory() -> usize {
    3
}

const fn default_max_chunk_bytes() -> usize {
    4_096
}

pub fn default_generic_terms() -> Vec<String> {
    [
        "a",
        "an",
        "and",
        "app",
        "application",
        "code",
        "dir",
        "directory",
        "for",
        "from",
        "game",
        "in",
        "last",
        "me",
        "my",
        "of",
        "on",
        "program",
        "project",
        "repo",
        "repository",
        "search",
        "site",
        "that",
        "the",
        "thing",
        "to",
        "tool",
        "web",
        "website",
        "with",
        "work",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn matches_name_pattern(pattern: &str, name: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();

    if pattern == name {
        return true;
    }

    if !pattern.contains('*') {
        return false;
    }

    wildcard_pattern_matches(&pattern, &name)
}

fn wildcard_pattern_matches(pattern: &str, name: &str) -> bool {
    let starts_anchored = !pattern.starts_with('*');
    let ends_anchored = !pattern.ends_with('*');
    let parts = pattern
        .split('*')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        return true;
    }

    let mut remaining = name;
    let mut part_index = 0;

    if starts_anchored {
        let first = parts[0];
        if !remaining.starts_with(first) {
            return false;
        }
        remaining = &remaining[first.len()..];
        part_index = 1;
    }

    let middle_end = if ends_anchored {
        parts.len().saturating_sub(1)
    } else {
        parts.len()
    };

    for part in &parts[part_index..middle_end] {
        let Some(found) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[found + part.len()..];
    }

    if ends_anchored {
        let last = parts[parts.len() - 1];
        return remaining.ends_with(last);
    }

    true
}

fn is_low_signal_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".xcassets")
        || lower.ends_with(".imageset")
        || lower.ends_with(".appiconset")
        || lower.ends_with(".colorset")
        || is_low_signal_asset_extension(&lower)
}

fn is_low_signal_asset_extension(name: &str) -> bool {
    matches!(
        name.rsplit_once('.').map(|(_, extension)| extension),
        Some(
            "svg"
                | "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "bmp"
                | "tif"
                | "tiff"
                | "pdf"
                | "zip"
                | "tar"
                | "gz"
                | "tgz"
                | "bz2"
                | "xz"
                | "7z"
                | "rar"
                | "dmg"
                | "mp3"
                | "mp4"
                | "mov"
                | "avi"
                | "webm"
                | "wav"
                | "flac"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
        )
    )
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

fn is_builtin_excluded_directory_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.strip_suffix(".localized").unwrap_or(lower.as_str()),
        "applications" | "library" | "network" | "system" | "volumes" | "cores" | "private"
    )
}

fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.contains("secret")
        || lower.contains("credential")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryTypeDefinition {
    pub label: String,
    #[serde(default)]
    pub rules: Vec<DirectoryTypeRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DirectoryTypeRule {
    pub id: String,
    pub confidence: f32,
    #[serde(default)]
    pub evidence_summary: Option<String>,
    #[serde(default)]
    pub signals: Vec<DirectoryTypeSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DirectoryTypeSignal {
    FileExists {
        path: String,
    },
    FileContains {
        path: String,
        #[serde(default)]
        contains_any: Vec<String>,
        #[serde(default)]
        contains_all: Vec<String>,
    },
    DirectoryName {
        #[serde(default)]
        equals_any: Vec<String>,
        #[serde(default)]
        contains_any: Vec<String>,
    },
    ChildName {
        #[serde(default)]
        contains_any: Vec<String>,
        #[serde(default)]
        starts_with_any: Vec<String>,
        #[serde(default)]
        ends_with_any: Vec<String>,
    },
    ChildFileContains {
        #[serde(default)]
        name_contains_any: Vec<String>,
        #[serde(default)]
        name_starts_with_any: Vec<String>,
        #[serde(default)]
        name_ends_with_any: Vec<String>,
        #[serde(default)]
        contains_any: Vec<String>,
        #[serde(default)]
        contains_all: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_round_trips_through_json() {
        let settings = Settings::default();
        let text = serde_json::to_string(&settings).unwrap();
        let parsed: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, settings);
    }

    #[test]
    fn older_json_config_defaults_missing_fields() {
        let parsed: Settings = serde_json::from_str(
            r#"
{
  "index": {
    "roots": ["~/Projects"],
    "exclude": [],
    "max_file_bytes": 65536,
    "max_excerpt_bytes": 4096,
    "max_entries_per_directory": 80
  }
}
"#,
        )
        .unwrap();

        assert_eq!(parsed.index.max_depth_per_top_level_directory, 3);
        assert_eq!(parsed.index.max_chunk_bytes, 4096);
        assert_eq!(parsed.index.generic_terms, default_generic_terms());
        assert_eq!(parsed.index.high_signal_files, default_high_signal_files());
        assert!(parsed.detectors.is_empty());
    }

    #[test]
    fn json_config_accepts_inline_detectors() {
        let parsed: Settings = serde_json::from_str(
            r#"
{
  "index": {
    "roots": ["~/Projects"],
    "exclude": [],
    "max_file_bytes": 65536,
    "max_excerpt_bytes": 4096,
    "max_entries_per_directory": 80
  },
  "detectors": [
    {
      "label": "custom project",
      "rules": [
        {
          "id": "marker",
          "confidence": 0.99,
          "signals": [
            { "kind": "file_exists", "path": "custom.marker" }
          ]
        }
      ]
    }
  ]
}
"#,
        )
        .unwrap();

        assert_eq!(parsed.detectors.len(), 1);
        assert_eq!(parsed.detectors[0].label, "custom project");
    }

    #[test]
    fn excludes_default_noise_and_secrets() {
        let index = IndexSettings::default();
        assert!(index.is_excluded_name(".git"));
        assert!(index.is_excluded_name(".env.local"));
        assert!(index.is_excluded_name("private.key"));
        assert!(index.is_excluded_name("Assets.xcassets"));
        assert!(index.is_excluded_name("Custom Icon.appiconset"));
        assert!(index.is_excluded_name("logo.svg"));
        assert!(index.is_excluded_name("screenshot.png"));
        assert!(index.is_excluded_name("archive.zip"));
        assert!(index.is_excluded_directory_name(".vscode"));
        assert!(index.is_excluded_directory_name("Applications"));
        assert!(index.is_excluded_directory_name("Applications.localized"));
        assert!(index.is_excluded_directory_name("Library"));
        assert!(index.is_excluded_directory_name("Library.localized"));
        assert!(index.is_excluded_directory_name("Network"));
        assert!(index.is_excluded_directory_name("System"));
        assert!(index.is_excluded_directory_name("System.localized"));
        assert!(index.is_excluded_directory_name("Volumes"));
        assert!(index.is_excluded_directory_name("cores"));
        assert!(index.is_excluded_directory_name("private"));
        assert!(!index.is_excluded_name(".vscode"));
        assert!(!index.is_excluded_name("README.md"));
    }

    #[test]
    fn high_signal_files_match_wildcards_and_exact_file_names() {
        let index = IndexSettings {
            high_signal_files: vec![
                "README*".to_string(),
                "*.md".to_string(),
                "debug.json".to_string(),
            ],
            ..IndexSettings::default()
        };

        assert!(index.is_high_signal_file_name("README.md"));
        assert!(index.is_high_signal_file_name("ARCHITECTURE.md"));
        assert!(index.is_high_signal_file_name("DEBUG.JSON"));
        assert!(!index.is_high_signal_file_name("debug.json.bak"));
    }

    #[test]
    fn high_signal_files_do_not_exclude_file_names() {
        let index = IndexSettings {
            high_signal_files: vec!["*.log".to_string(), "debug.json".to_string()],
            ..IndexSettings::default()
        };

        assert!(!index.is_excluded_name("server.log"));
        assert!(!index.is_excluded_name("debug.json"));
    }

    #[test]
    fn legacy_exclude_patterns_still_apply_to_file_names() {
        let index = IndexSettings {
            exclude: vec!["*.tmp".to_string()],
            ..IndexSettings::default()
        };

        assert!(index.is_excluded_name("notes.tmp"));
    }
}
