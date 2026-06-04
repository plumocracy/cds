mod error;
mod paths;
mod settings;

pub use error::ConfigError;
pub use paths::{AppPaths, expand_tilde};
pub use settings::{
    DirectoryTypeDefinition, DirectoryTypeRule, DirectoryTypeSignal, IndexSettings, Settings,
};
