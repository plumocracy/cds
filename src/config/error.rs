use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine {kind} directory; set {env_var}")]
    MissingDirectory {
        kind: &'static str,
        env_var: &'static str,
    },

    #[error("could not expand {value} because HOME is not set")]
    HomeNotSet { value: &'static str },

    #[error("failed to read config file {path}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse JSON config {path}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to create config directory {path}")]
    CreateConfigDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to serialize default config")]
    SerializeDefault {
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to write {path}")]
    WriteConfig {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
