use std::env;
use std::path::PathBuf;

use super::ConfigError;

pub type Result<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub database_file: PathBuf,
    pub cache_dir: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self> {
        let config_dir = env_path("CDS_CONFIG_DIR")
            .or_else(|| home_dir().map(|home| home.join(".config/cds")))
            .ok_or(ConfigError::MissingDirectory {
                kind: "config",
                env_var: "CDS_CONFIG_DIR",
            })?;

        let data_dir = env_path("CDS_DATA_DIR")
            .or_else(|| home_dir().map(|home| home.join(".local/share/cds")))
            .ok_or(ConfigError::MissingDirectory {
                kind: "data",
                env_var: "CDS_DATA_DIR",
            })?;

        let cache_dir = env_path("CDS_CACHE_DIR")
            .or_else(|| home_dir().map(|home| home.join(".cache/cds")))
            .ok_or(ConfigError::MissingDirectory {
                kind: "cache",
                env_var: "CDS_CACHE_DIR",
            })?;

        Ok(Self {
            config_file: config_dir.join("config.json"),
            database_file: data_dir.join("cds.sqlite"),
            config_dir,
            data_dir,
            cache_dir,
        })
    }
}

pub fn expand_tilde(value: &str) -> Result<PathBuf> {
    if value == "~" {
        return home_dir().ok_or(ConfigError::HomeNotSet { value: "~" });
    }

    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir()
            .map(|home| home.join(rest))
            .ok_or(ConfigError::HomeNotSet { value: "~/" });
    }

    Ok(PathBuf::from(value))
}

fn env_path(name: &str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_normal_paths_alone() {
        assert_eq!(
            expand_tilde("/tmp/projects").unwrap(),
            PathBuf::from("/tmp/projects")
        );
    }
}
