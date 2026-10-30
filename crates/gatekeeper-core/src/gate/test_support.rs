//! Fakes and helpers for state-machine tests (spec §22.1).
//!
//! Responsibility: an in-memory [`ProcessLauncher`] and [`HealthProbe`] so the
//! controller can be driven deterministically with `tokio::time::pause()`.
//! Compiled only for tests.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use super::controller::{self, ControllerHandle, Deps, StatusSnapshot};
use crate::config::schema::{Config, ModelConfig, ModelFiles};
use crate::events::EventBus;
use crate::ports::{
    ExitInfo, Health, HealthProbe, LaunchError, LaunchedProcess, MemoryStateStore, ProcessKiller, ProcessLauncher,
    StateStore, Upstream,
};
use crate::supervisor::footprint::memory_args_hash;
use crate::supervisor::log_capture::LogStore;

/// One fake child.
pub struct FakeProc {
    /// Fake pid (also used as the port).
    pub pid: u32,
    /// Fires the exit notification once.
    exit_tx: Mutex<Option<oneshot::Sender<ExitInfo>>>,
    /// Sends log lines to the controller.
    log_tx: mpsc::UnboundedSender<String>,
    /// True once the process is gone (killed or crashed).
    pub gone: AtomicBool,
    /// True if it was killed by the controller.
    pub killed: AtomicBool,
}

impl FakeProc {
    /// Ends the process as if it exited by itself.
    fn exit(&self, code: i32) {
        self.gone.store(true, Ordering::SeqCst);
        if let Some(tx) = self.exit_tx.lock().expect("lock").take() {
            let _ = tx.send(ExitInfo { code: Some(code), log_tail: Vec::new() });
        }
    }
}

/// Killer handed to the controller.
struct FakeKiller(Arc<FakeProc>);

impl ProcessKiller for FakeKiller {
    fn kill(&self) {
        self.0.killed.store(true, Ordering::SeqCst);
        self.0.exit(1);
    }
}

/// Scripted launcher.
#[derive(Default)]
pub struct FakeLauncher {
    /// All processes ever launched, per model.
    procs: Mutex<HashMap<String, Vec<Arc<FakeProc>>>>,
    /// Next fake pid.
    next_pid: AtomicU32,
    /// Port every child pretends to listen on; defaults to its pid.
    port_override: Mutex<Option<u16>>,
    /// Queued launch failures.
    failures: Mutex<VecDeque<String>>,
}

impl FakeLauncher {
    /// Makes all future children report this port (a stub server in tests).
    pub fn set_port(&self, port: u16) {
        *self.port_override.lock().expect("lock") = Some(port);
    }

    /// Makes the next launch fail with `message`.
    pub fn fail_next(&self, message: &str) {
        self.failures.lock().expect("lock").push_back(message.to_owned());
    }

    /// Number of launches for `model`.
    pub fn launches(&self, model: &str) -> usize {
        self.procs.lock().expect("lock").get(model).map_or(0, Vec::len)
    }

    /// The most recent process for `model`.
    pub fn last(&self, model: &str) -> Option<Arc<FakeProc>> {
        self.procs.lock().expect("lock").get(model).and_then(|v| v.last().cloned())
    }

    /// True if the latest process for `model` is still alive.
    pub fn running(&self, model: &str) -> bool {
        self.last(model).is_some_and(|p| !p.gone.load(Ordering::SeqCst))
    }

    /// Makes the latest process crash.
    pub fn crash(&self, model: &str) {
        if let Some(p) = self.last(model) {
            p.exit(-1);
        }
    }

    /// Emits a log line from the latest process.
    pub fn log(&self, model: &str, line: &str) {
        if let Some(p) = self.last(model) {
            let _ = p.log_tx.send(line.to_owned());
        }
    }
}

#[async_trait]
impl ProcessLauncher for FakeLauncher {
    async fn launch(&self, model: &ModelConfig) -> Result<LaunchedProcess, LaunchError> {
        // A real spawn is not instantaneous; yielding lets tests observe the
        // window between "starting" and "spawned".
        tokio::time::sleep(Duration::from_millis(1)).await;
        if let Some(msg) = self.failures.lock().expect("lock").pop_front() {
            return Err(LaunchError(msg));
        }
        let pid = 1000 + self.next_pid.fetch_add(1, Ordering::SeqCst);
        let (exit_tx, exited) = oneshot::channel();
        let (log_tx, logs) = mpsc::unbounded_channel();
        let proc = Arc::new(FakeProc {
            pid,
            exit_tx: Mutex::new(Some(exit_tx)),
            log_tx,
            gone: AtomicBool::new(false),
            killed: AtomicBool::new(false),
        });
        self.procs.lock().expect("lock").entry(model.id.clone()).or_default().push(proc.clone());
        Ok(LaunchedProcess {
            pid,
            upstream: Upstream {
                port: self.port_override.lock().expect("lock").unwrap_or_else(|| u16::try_from(pid).unwrap_or(1)),
                api_key: "k".into(),
            },
            exited,
            killer: Arc::new(FakeKiller(proc)),
            logs,
        })
    }
}

/// Probe whose readiness the test flips.
pub struct FakeProbe {
    /// Reported as `Ready` while true, `Loading` otherwise.
    pub ready: AtomicBool,
}

#[async_trait]
impl HealthProbe for FakeProbe {
    async fn check(&self, _upstream: &Upstream) -> Health {
        if self.ready.load(Ordering::SeqCst) { Health::Ready } else { Health::Loading }
    }
}

/// Everything a test needs.
pub struct Harness {
    /// Controller front door.
    pub handle: ControllerHandle,
    /// Fake launcher.
    pub launcher: Arc<FakeLauncher>,
    /// Fake probe.
    pub probe: Arc<FakeProbe>,
    /// State store.
    pub state: Arc<MemoryStateStore>,
    /// Event bus the controller publishes on.
    pub events: EventBus,
    /// Config in use (clone to modify).
    pub config: Config,
}

/// A model config with a given id and sensible test defaults.
pub fn model(id: &str) -> ModelConfig {
    ModelConfig {
        id: id.into(),
        files: ModelFiles { model: format!("{id}.gguf"), mmproj: None, draft: None },
        ..ModelConfig::default()
    }
}

/// A config with the given models and fast health polling.
pub fn config(ids: &[&str]) -> Config {
    let mut c = Config::default();
    c.advanced.health_poll_interval_ms = 10;
    c.games.cooldown_seconds = 30;
    c.models = ids.iter().map(|id| model(id)).collect();
    c
}

/// Starts a controller with fakes.
pub fn harness(config: Config) -> Harness {
    let launcher = Arc::new(FakeLauncher::default());
    let probe = Arc::new(FakeProbe { ready: AtomicBool::new(true) });
    let state = Arc::new(MemoryStateStore::default());
    let events = EventBus::new();
    let deps = Deps {
        launcher: launcher.clone(),
        probe: probe.clone(),
        state: state.clone(),
        events: events.clone(),
        logs: LogStore::new(),
    };
    let (handle, _join) = controller::spawn(Arc::new(config.clone()), deps);
    Harness { handle, launcher, probe, state, events, config }
}

impl Harness {
    /// Lets background tasks run and virtual time pass.
    pub async fn settle(&self) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// Snapshot of the whole system.
    pub async fn status(&self) -> StatusSnapshot {
        self.handle.status().await.expect("controller alive")
    }

    /// State name of one model.
    pub async fn state_of(&self, id: &str) -> &'static str {
        self.status().await.models.iter().find(|m| m.id == id).map(|m| m.state.as_str()).expect("model exists")
    }

    /// Records a known footprint so make-room decisions have data.
    pub fn learn_footprint(&self, model_id: &str, mib: u64) {
        let cfg = self.config.models.iter().find(|m| m.id == model_id).expect("model");
        self.state.set_footprint(model_id, &memory_args_hash(cfg), mib);
    }
}
