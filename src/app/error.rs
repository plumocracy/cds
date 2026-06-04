use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("index root is not valid UTF-8: {root:?}")]
    InvalidIndexRootUtf8 { root: OsString },

    #[error("search query is not valid UTF-8: {query:?}")]
    InvalidSearchQueryUtf8 { query: OsString },

    #[error("daemon socket I/O failed")]
    DaemonSocketIo {
        #[source]
        source: io::Error,
    },

    #[error("cds daemon is already running at {socket}")]
    DaemonAlreadyRunning { socket: PathBuf },

    #[error("cds daemon is unavailable for {mode}")]
    DaemonUnavailable { mode: &'static str },

    #[error("cds daemon did not return a dry-run report")]
    DaemonDryRunMissing,

    #[error("failed to run daemon process command `{command}`")]
    DaemonProcessCommand {
        command: &'static str,
        #[source]
        source: io::Error,
    },

    #[error("daemon process command `{command}` failed with status {status}")]
    DaemonProcessStatus {
        command: &'static str,
        status: String,
    },

    #[error("failed to resolve current executable for daemon restart")]
    ResolveCurrentExecutable {
        #[source]
        source: io::Error,
    },

    #[error("failed to start cds daemon")]
    StartDaemon {
        #[source]
        source: io::Error,
    },

    #[error("failed to parse daemon message")]
    ParseDaemonMessage {
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to serialize daemon message")]
    SerializeDaemonMessage {
        #[source]
        source: serde_json::Error,
    },

    #[error("failed to inspect daemon watch path {path}")]
    InspectDaemonWatchPath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}
