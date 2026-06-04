use thiserror::Error;

use crate::db::DbError;
use crate::embed::EmbedError;

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("failed to embed query")]
    EmbedQuery {
        #[source]
        source: EmbedError,
    },

    #[error("failed to load indexed directories")]
    LoadDirectories {
        #[source]
        source: DbError,
    },
}
