use std::io;

use thiserror::Error;

use crate::app::AppError;
use crate::config::ConfigError;
use crate::db::DbError;
use crate::embed::EmbedError;
use crate::index::IndexError;
use crate::search::SearchError;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    App(Box<AppError>),

    #[error(transparent)]
    Config(Box<ConfigError>),

    #[error(transparent)]
    Db(Box<DbError>),

    #[error(transparent)]
    Embed(Box<EmbedError>),

    #[error(transparent)]
    Index(Box<IndexError>),

    #[error(transparent)]
    Search(Box<SearchError>),

    #[error("failed to write stdout")]
    Stdout(#[source] io::Error),

    #[error("failed to read stdin")]
    Stdin(#[source] io::Error),
}

impl From<AppError> for Error {
    fn from(error: AppError) -> Self {
        Self::App(Box::new(error))
    }
}

impl From<ConfigError> for Error {
    fn from(error: ConfigError) -> Self {
        Self::Config(Box::new(error))
    }
}

impl From<DbError> for Error {
    fn from(error: DbError) -> Self {
        Self::Db(Box::new(error))
    }
}

impl From<EmbedError> for Error {
    fn from(error: EmbedError) -> Self {
        Self::Embed(Box::new(error))
    }
}

impl From<IndexError> for Error {
    fn from(error: IndexError) -> Self {
        Self::Index(Box::new(error))
    }
}

impl From<SearchError> for Error {
    fn from(error: SearchError) -> Self {
        Self::Search(Box::new(error))
    }
}

pub fn app_err(error: AppError) -> Error {
    Error::from(error)
}

pub fn config_err(error: ConfigError) -> Error {
    Error::from(error)
}

pub fn db_err(error: DbError) -> Error {
    Error::from(error)
}

pub fn embed_err(error: EmbedError) -> Error {
    Error::from(error)
}

pub fn index_err(error: IndexError) -> Error {
    Error::from(error)
}

pub fn search_err(error: SearchError) -> Error {
    Error::from(error)
}
