//! Error types shared across the core crate.
//!
//! Responsibility: one `CoreError` for fallible operations that cross module
//! boundaries. Modules with richer failure modes (launching, platform calls)
//! define their own error and convert into this one.
//! Not responsible for: HTTP-facing errors (see `proxy::errors`).

use std::path::PathBuf;

/// Convenient result alias for the crate.
pub type CoreResult<T> = Result<T, CoreError>;

/// Failures that can occur in core operations.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// A file could not be read or written.
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The path involved.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// JSON could not be parsed or produced.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The configuration failed validation.
    #[error("configuration is invalid ({0} problem(s))")]
    InvalidConfig(usize),
    /// The configuration file was written by a newer, incompatible version.
    #[error("config schema_version {found} is newer than supported version {supported}")]
    ConfigTooNew {
        /// Version found in the file.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },
    /// A model could not be launched.
    #[error("failed to launch model: {0}")]
    Launch(String),
    /// A platform (Win32) call failed.
    #[error("platform error: {0}")]
    Platform(String),
    /// Anything else, with a message for the log.
    #[error("{0}")]
    Other(String),
}

impl CoreError {
    /// Wraps an I/O error together with the path it concerned.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }
}
