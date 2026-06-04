use std::ffi::OsString;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("index root is not valid UTF-8: {root:?}")]
    InvalidIndexRootUtf8 { root: OsString },

    #[error("search query is not valid UTF-8: {query:?}")]
    InvalidSearchQueryUtf8 { query: OsString },
}
