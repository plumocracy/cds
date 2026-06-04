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
            .any(|excluded| matches_exclude_pattern(excluded, name))
            || is_low_signal_name(name)
            || is_secret_name(name)
    }

    pub fn is_excluded_directory_name(&self, name: &str) -> bool {
        is_hidden_name(name) || self.is_excluded_name(name)
    }
}

impl Default for IndexSettings {
    fn default() -> Self {
        Self {
            roots: vec!["~/Projects".to_string()],
            exclude: vec![
                ".git".to_string(),
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
                "*.svg".to_string(),
                "*.png".to_string(),
                "*.jpg".to_string(),
                "*.jpeg".to_string(),
                "*.gif".to_string(),
                "*.webp".to_string(),
                "*.ico".to_string(),
                "*.pdf".to_string(),
                "*.zip".to_string(),
            ],
            max_file_bytes: 65_536,
            max_excerpt_bytes: 4_096,
            max_entries_per_directory: 80,
            max_depth_per_top_level_directory: default_max_depth_per_top_level_directory(),
            max_chunk_bytes: default_max_chunk_bytes(),
            generic_terms: default_generic_terms(),
        }
    }
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

fn matches_exclude_pattern(pattern: &str, name: &str) -> bool {
    if pattern == name {
        return true;
    }

    let pattern = pattern.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();

    if let Some(suffix) = pattern.strip_prefix('*') {
        return name.ends_with(suffix);
    }

    pattern == name
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
        assert!(!index.is_excluded_name(".vscode"));
        assert!(!index.is_excluded_name("README.md"));
    }
}
