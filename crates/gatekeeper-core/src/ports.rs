//! Traits for the outside world the core depends on (spec §22.1).
//!
//! Responsibility: define the seams where tests substitute fakes for
//! processes, health checks and persistence, so the state machines run
//! without Windows, a GPU or a real llama-server.
//! Not responsible for: the real implementations (`supervisor`, `platform`).

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::config::schema::ModelConfig;
use crate::sync::MutexExt;

/// Where a running llama-server listens, and the key it requires.
#[derive(Clone, PartialEq, Eq)]
pub struct Upstream {
    /// Loopback TCP port.
    pub port: u16,
    /// Per-launch API key (spec D4). Never logged.
    pub api_key: String,
}

impl std::fmt::Debug for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Upstream").field("port", &self.port).field("api_key", &"<redacted>").finish()
    }
}

/// How a child process ended.
#[derive(Debug, Clone, Default)]
pub struct ExitInfo {
    /// Exit code, if the OS reported one.
    pub code: Option<i32>,
    /// Last log lines, attached to crash events (spec §9.3).
    pub log_tail: Vec<String>,
}

/// Terminates a child process. Must be idempotent and must not block.
pub trait ProcessKiller: Send + Sync {
    /// Kills the process (TerminateProcess on Windows). Calling it again, or
    /// after the process already exited, does nothing.
    fn kill(&self);
}

/// A freshly started child process.
pub struct LaunchedProcess {
    /// OS process id, used to attribute GPU memory.
    pub pid: u32,
    /// Where it listens.
    pub upstream: Upstream,
    /// Resolves once, when the process has exited.
    pub exited: oneshot::Receiver<ExitInfo>,
    /// Handle that kills it.
    pub killer: Arc<dyn ProcessKiller>,
    /// Combined stdout/stderr lines as they are produced.
    pub logs: mpsc::UnboundedReceiver<String>,
}

/// A launch that could not even start the process.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct LaunchError(pub String);

/// Starts llama-server children.
#[async_trait]
pub trait ProcessLauncher: Send + Sync {
    /// Spawns a child for `model` with its configured arguments.
    ///
    /// # Errors
    /// Returns [`LaunchError`] if the executable is missing, the arguments
    /// are rejected, or the spawn fails.
    async fn launch(&self, model: &ModelConfig) -> Result<LaunchedProcess, LaunchError>;
}

/// Result of one `/health` check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// HTTP 200: the model is loaded and serving.
    Ready,
    /// HTTP 503: still loading.
    Loading,
    /// No answer: not listening yet, or gone.
    Down,
}

/// Checks whether a child is ready.
#[async_trait]
pub trait HealthProbe: Send + Sync {
    /// Performs one health check against `upstream`.
    async fn check(&self, upstream: &Upstream) -> Health;
}

/// Persistent learned data that is not a setting (`state.json`, spec D7).
pub trait StateStore: Send + Sync {
    /// Measured footprint in MiB for `model` launched with memory-relevant
    /// settings identified by `args_hash`.
    fn footprint(&self, model: &str, args_hash: &str) -> Option<u64>;
    /// Stores a measured footprint.
    fn set_footprint(&self, model: &str, args_hash: &str, mib: u64);
}

/// Secure storage for secrets (Windows Credential Manager in production;
/// spec D7). Secrets never appear in `config.json`.
pub trait SecretStore: Send + Sync {
    /// Reads a secret by name, e.g. `lan_api_key` or `hf_token`.
    fn get(&self, name: &str) -> Option<String>;
    /// Stores a secret.
    ///
    /// # Errors
    /// Returns a message if the secret cannot be stored.
    fn set(&self, name: &str, value: &str) -> Result<(), String>;
    /// Removes a secret; removing a missing one is not an error.
    fn clear(&self, name: &str);
}

/// A [`SecretStore`] kept in memory; used by tests and the headless CLI.
#[derive(Default)]
pub struct MemorySecretStore {
    /// `name -> value`.
    map: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

impl SecretStore for MemorySecretStore {
    fn get(&self, name: &str) -> Option<String> {
        self.map.lock_ok().get(name).cloned()
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        self.map.lock_ok().insert(name.to_owned(), value.to_owned());
        Ok(())
    }

    fn clear(&self, name: &str) {
        self.map.lock_ok().remove(name);
    }
}

/// A [`StateStore`] that keeps everything in memory; used by tests and as a
/// fallback when `state.json` cannot be used.
#[derive(Default)]
pub struct MemoryStateStore {
    /// `(model, hash) -> MiB`.
    map: std::sync::Mutex<std::collections::HashMap<(String, String), u64>>,
}

impl StateStore for MemoryStateStore {
    fn footprint(&self, model: &str, args_hash: &str) -> Option<u64> {
        let map = self.map.lock_ok();
        map.get(&(model.to_owned(), args_hash.to_owned())).copied()
    }

    fn set_footprint(&self, model: &str, args_hash: &str, mib: u64) {
        let mut map = self.map.lock_ok();
        map.insert((model.to_owned(), args_hash.to_owned()), mib);
    }
}
