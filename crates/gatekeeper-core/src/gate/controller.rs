//! The gate controller: a single actor that owns all lifecycle state
//! (spec §5.2, §7).
//!
//! Responsibility: every decision about which models run happens here, one
//! message at a time, so races between games, VRAM samples, requests and
//! process exits cannot interleave. Other components send [`Cmd`] messages
//! (usually through [`ControllerHandle`]) and receive verdicts over oneshot
//! channels.
//! Not responsible for: HTTP (`proxy`), process mechanics
//! (`ports::ProcessLauncher`), or sampling the GPU (`gpu`).
//!
//! Implements FR-GAME-2..7, FR-VRAM-2..7, FR-REQ-3, FR-APP-3.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::drain::{self, DrainPlan};
use super::gate_state::{GateState, SignalEffect};
use super::model_state::{GateIndicator, ModelRt, ModelState, ModelStateKind, Run, UnloadCause, Waiter};
use super::rejection::{RejectKind, Rejection};
use super::vram_rules::{
    BASELINE_DELAY, Candidate, PressureTracker, RELEASE_FRACTION_PERCENT, layers_overflow, pick_make_room_victims,
    pick_pressure_victims, pressure_percent,
};
use crate::config::schema::{Config, GamePolicy, ModelConfig, OverflowAction};
use crate::detect::GameSignal;
use crate::events::{AppEvent, EventBus, NotificationKind};
use crate::gpu::VramSample;
use crate::ports::{ExitInfo, HealthProbe, LaunchedProcess, ProcessKiller, ProcessLauncher, StateStore, Upstream};
use crate::proxy::inflight::{CancelCause, CancelHandle, CancelInfo, InflightGuard};
use crate::supervisor::footprint::{estimate_for, memory_args_hash};
use crate::supervisor::log_capture::LogStore;
use crate::supervisor::log_parse::{is_bind_failure, parse_layer_placement};

/// How often idle timers are checked. Idle TTL is measured in minutes, so a
/// 10-second tick is precise enough and costs nothing when idle (NFR-1).
const TICK_INTERVAL: Duration = Duration::from_secs(10);

/// First load-failure backoff; doubles per consecutive failure up to
/// `advanced.load_failure_backoff_max_seconds` (spec §9.3).
const BACKOFF_BASE_SECONDS: u64 = 10;

/// `Retry-After` for a model held by the loop guard (spec §8.5).
const VRAM_GUARD_RETRY_SECONDS: u32 = 15;

/// Extra time the start task allows beyond clients' `load_timeout` before
/// declaring the start failed.
const START_GRACE: Duration = Duration::from_secs(5);

/// Restarts allowed after a port collision before treating it as a failure.
const MAX_PORT_RETRIES: u32 = 2;

/// Log lines attached to a failure message.
const FAILURE_LOG_LINES: usize = 20;

// ------------------------------------------------------------------ types

/// The verdict for an admitted request: where to send it and how to track
/// and cancel it.
#[derive(Debug)]
pub struct Admitted {
    /// Canonical model id.
    pub model: String,
    /// Child to forward to.
    pub upstream: Upstream,
    /// Keeps the request counted until dropped.
    pub guard: InflightGuard,
    /// Fires if the controller must cancel this request.
    pub cancel: CancelHandle,
}

/// Collaborators the controller needs.
#[derive(Clone)]
pub struct Deps {
    /// Starts children.
    pub launcher: Arc<dyn ProcessLauncher>,
    /// Checks readiness.
    pub probe: Arc<dyn HealthProbe>,
    /// Persists learned footprints.
    pub state: Arc<dyn StateStore>,
    /// Event bus.
    pub events: EventBus,
    /// Recent log lines per model.
    pub logs: LogStore,
}

/// Events produced by a model's start/exit tasks.
pub(crate) enum ModelEvent {
    /// Process created.
    Spawned { model: String, generation: u64, pid: u32, upstream: Upstream, killer: Arc<dyn ProcessKiller> },
    /// Process could not be created.
    SpawnFailed { model: String, generation: u64, error: String },
    /// `/health` returned 200.
    Ready { model: String, generation: u64 },
    /// The model did not become ready in time.
    StartTimedOut { model: String, generation: u64 },
    /// Process ended.
    Exited { model: String, generation: u64, info: ExitInfo },
    /// GPU layer placement parsed from the log.
    Layers { model: String, generation: u64, offloaded: u32, total: u32 },
}

/// Messages the controller understands.
pub(crate) enum Cmd {
    /// A client request needs a verdict.
    Admit { name: String, reply: oneshot::Sender<Result<Admitted, Rejection>> },
    /// A request finished (sent by [`InflightGuard`]'s `Drop`).
    InflightDone { model: String, request_id: u64 },
    /// A detector reported.
    Game(GameSignal),
    /// New GPU reading.
    Vram(VramSample),
    /// Start/exit task event.
    Model(ModelEvent),
    /// A queued request waited too long.
    WaiterTimeout { model: String, waiter: u64 },
    /// The post-game cooldown elapsed.
    CooldownElapsed { id: u64 },
    /// The drain wait elapsed.
    DrainTimeout { model: String, timer: u64 },
    /// The VRAM-release wait elapsed.
    ReleaseTimeout { model: String, timer: u64 },
    /// New configuration.
    SetConfig(Arc<Config>),
    /// Load a model without a request (preload, "Load" button).
    Load { id: String, reply: oneshot::Sender<Result<(), Rejection>> },
    /// Unload a model, optionally holding it.
    Unload { id: String, hold: bool, reply: oneshot::Sender<()> },
    /// Restart a model (settings changed).
    Restart { id: String, reply: oneshot::Sender<()> },
    /// Unload everything now.
    UnloadAll,
    /// The PC woke from sleep: re-publish state and health-check loaded models.
    Resumed,
    /// A model did not answer its health check after a resume.
    Unhealthy { model: String, generation: u64 },
    /// "Allow anyway" for the current game session.
    AllowAnyway,
    /// Release a held model.
    ReleaseHold { id: String },
    /// Pause or resume the gatekeeper.
    SetPaused(bool),
    /// Status snapshot.
    Status { reply: oneshot::Sender<StatusSnapshot> },
    /// Kill everything and stop.
    Shutdown { reply: oneshot::Sender<()> },
}

/// Status of one model for the UI and admin API.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    /// Model id.
    pub id: String,
    /// Friendly name.
    pub display_name: String,
    /// Lifecycle state.
    pub state: ModelStateKind,
    /// Unload cause while draining/stopping.
    pub cause: Option<UnloadCause>,
    /// Why new requests are refused right now, if they are.
    pub blocked_reason: Option<RejectKind>,
    /// Requests being served.
    pub inflight: usize,
    /// Requests waiting for readiness.
    pub queued: usize,
    /// VRAM footprint in MiB.
    pub footprint_mib: Option<u64>,
    /// True if `footprint_mib` is an estimate.
    pub footprint_estimated: bool,
    /// GPU layers `(offloaded, total)`.
    pub gpu_layers: Option<(u32, u32)>,
    /// Overflow description, if detected.
    pub overflow: Option<String>,
    /// Settings changed since launch.
    pub needs_restart: bool,
    /// Held by the user.
    pub manual_hold: bool,
    /// Process id.
    pub pid: Option<u32>,
    /// Seconds since last use.
    pub idle_seconds: u64,
    /// Failure message while in backoff.
    pub failure: Option<String>,
}

/// Whole-system status.
#[derive(Debug, Clone, Serialize)]
pub struct StatusSnapshot {
    /// The gate in one word; the tray icon and the window both show this.
    pub gate: GateIndicator,
    /// User paused the gatekeeper.
    pub paused: bool,
    /// Most recent game name.
    pub game: Option<String>,
    /// Detector that saw it.
    pub game_source: Option<String>,
    /// A game session is open.
    pub session_active: bool,
    /// "Allow anyway" is in effect.
    pub override_active: bool,
    /// Cooldown running.
    pub cooling_down: bool,
    /// Per-model status.
    pub models: Vec<ModelStatus>,
    /// Latest GPU reading.
    pub vram: Option<VramSample>,
}

impl StatusSnapshot {
    /// True while a game keeps the gate closed (a session is open and not overridden).
    pub fn gate_closed(&self) -> bool {
        self.session_active && !self.override_active
    }
}

// ----------------------------------------------------------------- handle

/// Cloneable front door to the controller.
#[derive(Clone)]
pub struct ControllerHandle {
    /// Channel into the actor.
    tx: mpsc::UnboundedSender<Cmd>,
}

impl ControllerHandle {
    /// Asks whether a request for `model` may proceed. Waits for the model
    /// to load if necessary; the controller applies the load timeout.
    ///
    /// # Errors
    /// Returns the [`Rejection`] explaining why the request cannot proceed.
    pub async fn admit(&self, model: &str) -> Result<Admitted, Rejection> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Admit { name: model.to_owned(), reply }).is_err() {
            return Err(shutting_down(model));
        }
        rx.await.unwrap_or_else(|_| Err(shutting_down(model)))
    }

    /// Reports a detector verdict.
    pub fn game_signal(&self, signal: GameSignal) {
        let _ = self.tx.send(Cmd::Game(signal));
    }

    /// Reports a GPU reading.
    pub fn vram_sample(&self, sample: VramSample) {
        let _ = self.tx.send(Cmd::Vram(sample));
    }

    /// Applies a new configuration.
    pub fn set_config(&self, config: Arc<Config>) {
        let _ = self.tx.send(Cmd::SetConfig(config));
    }

    /// Starts loading a model without a request.
    ///
    /// # Errors
    /// Returns the rejection if the gate forbids loading it.
    pub async fn load(&self, id: &str) -> Result<(), Rejection> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Load { id: id.to_owned(), reply }).is_err() {
            return Err(shutting_down(id));
        }
        rx.await.unwrap_or_else(|_| Err(shutting_down(id)))
    }

    /// Unloads a model; with `hold`, rejects requests until released.
    pub async fn unload(&self, id: &str, hold: bool) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Unload { id: id.to_owned(), hold, reply }).is_ok() {
            let _ = rx.await;
        }
    }

    /// Restarts a model.
    pub async fn restart(&self, id: &str) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Restart { id: id.to_owned(), reply }).is_ok() {
            let _ = rx.await;
        }
    }

    /// Tells the controller the PC woke from sleep (spec §21): state is
    /// published again, which makes the sampler read the GPU at once, and every
    /// loaded model is health-checked and restarted if it no longer answers.
    pub fn system_resumed(&self) {
        let _ = self.tx.send(Cmd::Resumed);
    }

    /// Unloads every model.
    pub fn unload_all(&self) {
        let _ = self.tx.send(Cmd::UnloadAll);
    }

    /// Chooses "allow anyway" for the current game session.
    pub fn allow_anyway(&self) {
        let _ = self.tx.send(Cmd::AllowAnyway);
    }

    /// Releases a held model.
    pub fn release_hold(&self, id: &str) {
        let _ = self.tx.send(Cmd::ReleaseHold { id: id.to_owned() });
    }

    /// Pauses or resumes the gatekeeper.
    pub fn set_paused(&self, paused: bool) {
        let _ = self.tx.send(Cmd::SetPaused(paused));
    }

    /// Takes a status snapshot, or `None` if the controller has stopped.
    pub async fn status(&self) -> Option<StatusSnapshot> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Status { reply }).ok()?;
        rx.await.ok()
    }

    /// Kills all children and stops the controller.
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::Shutdown { reply }).is_ok() {
            let _ = rx.await;
        }
    }
}

/// The span over which VRAM unloads count toward the loop guard.
fn loop_guard_window(cfg: &Config) -> Duration {
    Duration::from_secs(u64::from(cfg.vram.loop_guard.window_minutes) * 60)
}

/// Rejection used when the controller is gone.
fn shutting_down(model: &str) -> Rejection {
    Rejection::new(RejectKind::ShuttingDown, model, "The gatekeeper is shutting down.")
}

/// Starts the controller task.
pub fn spawn(config: Arc<Config>, deps: Deps) -> (ControllerHandle, JoinHandle<()>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let now = Instant::now();
    let mut models = HashMap::new();
    for m in &config.models {
        models.insert(m.id.clone(), ModelRt::new(now));
    }
    let mut controller = Controller {
        cfg: config,
        models,
        gate: GateState::default(),
        deps,
        tx: tx.clone(),
        rx,
        last_sample: None,
        pressure: PressureTracker::default(),
        counter: 0,
    };
    // Implements FR-GAME-6 (preload): models marked `preload_on_start` load now.
    controller.preload(|m| m.lifecycle.preload_on_start);
    let join = tokio::spawn(controller.run());
    (ControllerHandle { tx }, join)
}

// ------------------------------------------------------------- controller

/// The actor's state.
struct Controller {
    /// Active configuration.
    cfg: Arc<Config>,
    /// Runtime record per model id.
    models: HashMap<String, ModelRt>,
    /// Global gate.
    gate: GateState,
    /// Collaborators.
    deps: Deps,
    /// Sender clone for timers and tasks.
    tx: mpsc::UnboundedSender<Cmd>,
    /// Inbox.
    rx: mpsc::UnboundedReceiver<Cmd>,
    /// Most recent GPU reading.
    last_sample: Option<VramSample>,
    /// Pressure persistence tracker.
    pressure: PressureTracker,
    /// Source of unique ids (generations, requests, waiters, timers).
    counter: u64,
}

impl Controller {
    /// Main loop: process messages until shutdown or all handles drop.
    async fn run(mut self) {
        let mut tick = tokio::time::interval(TICK_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                cmd = self.rx.recv() => match cmd {
                    Some(cmd) => if self.handle(cmd) { break; },
                    None => break,
                },
                _ = tick.tick() => self.on_tick(),
            }
        }
        debug!("gate controller stopped");
    }

    /// Returns a fresh unique id.
    fn next_id(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Dispatches one message. Returns true to stop the loop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Admit { name, reply } => self.admit(&name, reply),
            Cmd::InflightDone { model, request_id } => self.on_inflight_done(&model, request_id),
            Cmd::Game(sig) => self.on_game(sig),
            Cmd::Vram(sample) => self.on_vram(sample),
            Cmd::Model(ev) => self.on_model_event(ev),
            Cmd::WaiterTimeout { model, waiter } => self.on_waiter_timeout(&model, waiter),
            Cmd::CooldownElapsed { id } => self.on_cooldown_elapsed(id),
            Cmd::DrainTimeout { model, timer } => self.on_drain_timeout(&model, timer),
            Cmd::ReleaseTimeout { model, timer } => self.on_release_timeout(&model, timer),
            Cmd::SetConfig(cfg) => self.on_set_config(cfg),
            Cmd::Load { id, reply } => {
                let _ = reply.send(self.load(&id));
            }
            Cmd::Unload { id, hold, reply } => {
                self.user_unload(&id, hold);
                let _ = reply.send(());
            }
            Cmd::Restart { id, reply } => {
                self.restart(&id);
                let _ = reply.send(());
            }
            Cmd::Resumed => self.after_resume(),
            Cmd::Unhealthy { model, generation } => {
                if self.is_current_in(&model, generation, &ModelState::Ready) {
                    warn!(model = %model, "model does not answer after the PC woke up; restarting it");
                    self.restart(&model);
                }
            }
            Cmd::UnloadAll => {
                for id in self.model_ids() {
                    self.begin_unload(&id, UnloadCause::User);
                }
            }
            Cmd::AllowAnyway => {
                self.gate.allow_anyway();
                self.publish_gate();
            }
            Cmd::ReleaseHold { id } => self.release_hold(&id),
            Cmd::SetPaused(p) => self.set_paused(p),
            Cmd::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Cmd::Shutdown { reply } => {
                self.shutdown();
                let _ = reply.send(());
                return true;
            }
        }
        false
    }

    /// Ids of all tracked models.
    fn model_ids(&self) -> Vec<String> {
        self.models.keys().cloned().collect()
    }

    /// Configuration of the model with canonical id `id`.
    fn model_cfg(&self, id: &str) -> Option<&ModelConfig> {
        self.cfg.models.iter().find(|m| m.id == id)
    }

    // --------------------------------------------------------- transitions

    /// Changes a model's state, logging and publishing the transition
    /// (spec §20: every transition is logged at info).
    fn set_state(&mut self, id: &str, new: ModelState) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        // A restart after a port collision re-enters `Starting`; that is not
        // a transition worth reporting.
        if rt.state == new {
            return;
        }
        let (from, to, cause) = (rt.state.kind(), new.kind(), new.cause());
        info!(
            model = id,
            from = from.as_str(),
            to = to.as_str(),
            cause = cause.map(UnloadCause::as_str),
            "model state"
        );
        self.deps.events.publish(AppEvent::ModelState { model: id.to_owned(), from, to, cause });
        rt.state = new;
    }

    /// Publishes the gate state.
    fn publish_gate(&self) {
        self.deps.events.publish(AppEvent::Gate {
            paused: self.gate.paused,
            game: self.gate.active_game().map(|(n, _)| n.to_owned()),
            override_active: self.gate.override_active(),
        });
    }

    /// Sends a notification event.
    fn notify(&self, kind: NotificationKind, title: &str, body: String) {
        self.deps.events.publish(AppEvent::Notification { kind, title: title.to_owned(), body });
    }

    // ------------------------------------------------------------ admission

    /// Why `id` is blocked by the gate right now, if it is (spec §7.1).
    fn gate_block(&self, id: &str) -> Option<Rejection> {
        let retry = self.cfg.errors.retry_after_seconds;
        if self.gate.paused {
            return Some(self.paused_rejection(id));
        }
        let mc = self.model_cfg(id)?;
        if self.gate.game_blocks() && mc.lifecycle.game_policy == GamePolicy::Unload {
            return Some(self.game_rejection(id));
        }
        if self.models.get(id).is_some_and(|rt| rt.manual_hold) {
            return Some(
                Rejection::new(RejectKind::Paused, id, format!("Model '{id}' was unloaded and held by the user."))
                    .with_detail(json!({ "held_by_user": true }))
                    .with_retry_after(retry),
            );
        }
        None
    }

    /// The "a game is running" rejection.
    fn game_rejection(&self, id: &str) -> Rejection {
        let (name, source) = self.gate.current_game().unwrap_or(("a game", "unknown"));
        Rejection::new(
            RejectKind::GameRunning,
            id,
            format!("Model '{id}' is not loaded because a game is running ({name}). Retry after the game closes."),
        )
        .with_detail(json!({ "game": name, "source": source }))
        .with_retry_after(self.cfg.errors.retry_after_seconds)
    }

    /// Rejection to give queued requests when an unload with `cause` begins.
    fn cause_rejection(&self, id: &str, cause: UnloadCause) -> Rejection {
        match cause {
            UnloadCause::Game => self.game_rejection(id),
            UnloadCause::Pause => self.paused_rejection(id),
            _ => shutting_down(id),
        }
    }

    /// The "gatekeeper is paused" rejection.
    fn paused_rejection(&self, id: &str) -> Rejection {
        Rejection::new(RejectKind::Paused, id, format!("Model '{id}' is unavailable because the gatekeeper is paused."))
            .with_retry_after(self.cfg.errors.retry_after_seconds)
    }

    /// Implements the admission algorithm of spec §7.3.
    fn admit(&mut self, name: &str, reply: oneshot::Sender<Result<Admitted, Rejection>>) {
        let Some(mc) = self.cfg.find_model(name) else {
            let _ = reply.send(Err(Rejection::new(
                RejectKind::UnknownModel,
                name,
                format!("No enabled model named '{name}'."),
            )));
            return;
        };
        let id = mc.id.clone();
        let load_timeout = Duration::from_secs(u64::from(mc.lifecycle.load_timeout_seconds));
        self.models.entry(id.clone()).or_insert_with(|| ModelRt::new(Instant::now()));

        if let Some(rejection) = self.gate_block(&id) {
            let _ = reply.send(Err(rejection));
            return;
        }

        let state = self.models[&id].state.clone();
        match state {
            ModelState::Held => {
                let _ = reply.send(Err(self.guard_rejection(&id)));
            }
            ModelState::Ready => {
                let admitted = self.issue(&id);
                let _ = reply.send(admitted);
            }
            ModelState::Failed { until, message } if Instant::now() < until => {
                let remaining = until.saturating_duration_since(Instant::now()).as_secs() + 1;
                let _ = reply.send(Err(Rejection::new(
                    RejectKind::ModelLoadFailed,
                    &id,
                    format!("Model '{id}' failed to start and is backing off: {message}"),
                )
                .with_retry_after(u32::try_from(remaining).unwrap_or(u32::MAX))));
            }
            ModelState::Failed { .. } => {
                // Backoff over: behave as if unloaded (spec §7.2 Failed -> Unloaded).
                self.set_state(&id, ModelState::Unloaded);
                self.enqueue(&id, reply, load_timeout);
                self.maybe_start(&id);
            }
            ModelState::Unloaded => {
                self.enqueue(&id, reply, load_timeout);
                self.maybe_start(&id);
            }
            // Starting: wait for ready. Draining/Stopping with a non-rejecting
            // cause: wait for the unload to finish, then reload (FR-VRAM-3).
            ModelState::Starting | ModelState::Draining(_) | ModelState::Stopping(_) => {
                self.enqueue(&id, reply, load_timeout);
            }
        }
    }

    /// Rejection for a model held by the loop guard.
    fn guard_rejection(&self, id: &str) -> Rejection {
        Rejection::new(
            RejectKind::VramGuard,
            id,
            format!("Model '{id}' is held because it was unloaded repeatedly for VRAM reasons. It will be released when enough VRAM is free."),
        )
        .with_retry_after(VRAM_GUARD_RETRY_SECONDS)
    }

    /// Queues a request until its model is ready, with a deadline.
    fn enqueue(&mut self, id: &str, reply: oneshot::Sender<Result<Admitted, Rejection>>, timeout: Duration) {
        let waiter = self.next_id();
        if let Some(rt) = self.models.get_mut(id) {
            rt.waiters.push(Waiter { id: waiter, reply });
            rt.last_used = Instant::now();
        }
        self.send_after(timeout, Cmd::WaiterTimeout { model: id.to_owned(), waiter });
    }

    /// Delivers `cmd` back to the controller loop after `delay`.
    fn send_after(&self, delay: Duration, cmd: Cmd) {
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let _ = tx.send(cmd);
        });
    }

    /// Registers a request against a ready model and builds its verdict.
    fn issue(&mut self, id: &str) -> Result<Admitted, Rejection> {
        let request_id = self.next_id();
        let tx = self.tx.clone();
        let Some(rt) = self.models.get_mut(id) else {
            return Err(Rejection::new(RejectKind::UnknownModel, id, "Unknown model."));
        };
        let Some(upstream) = rt.run.as_ref().and_then(|r| r.upstream.clone()) else {
            return Err(Rejection::new(RejectKind::ModelCrashed, id, "The model process is not available."));
        };
        let cancel = CancelHandle::new();
        rt.inflight.insert(request_id, cancel.clone());
        rt.last_used = Instant::now();
        Ok(Admitted {
            model: id.to_owned(),
            upstream,
            guard: InflightGuard::new(Arc::from(id), request_id, tx),
            cancel,
        })
    }

    /// Answers every queued request with a success verdict.
    fn resolve_waiters(&mut self, id: &str) {
        let waiters = match self.models.get_mut(id) {
            Some(rt) => std::mem::take(&mut rt.waiters),
            None => return,
        };
        for w in waiters {
            let admitted = self.issue(id);
            let _ = w.reply.send(admitted);
        }
    }

    /// Answers every queued request with `rejection`.
    fn reject_waiters(&mut self, id: &str, rejection: &Rejection) {
        let waiters = match self.models.get_mut(id) {
            Some(rt) => std::mem::take(&mut rt.waiters),
            None => return,
        };
        for w in waiters {
            let _ = w.reply.send(Err(rejection.clone()));
        }
    }

    /// A queued request hit its deadline.
    fn on_waiter_timeout(&mut self, id: &str, waiter: u64) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        let Some(pos) = rt.waiters.iter().position(|w| w.id == waiter) else {
            return;
        };
        let w = rt.waiters.remove(pos);
        let _ = w.reply.send(Err(Rejection::new(
            RejectKind::LoadTimeout,
            id,
            format!("Model '{id}' did not become ready in time."),
        )));
    }

    /// A request ended.
    fn on_inflight_done(&mut self, id: &str, request_id: u64) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        rt.inflight.remove(&request_id);
        rt.last_used = Instant::now();
        // Drain complete: nothing left to wait for (FR-GAME-4).
        if let ModelState::Draining(cause) = rt.state
            && rt.inflight.is_empty()
        {
            self.enter_stopping(id, cause);
        }
    }

    // ------------------------------------------------------------- starting

    /// Starts a model if the gate and VRAM allow, otherwise makes room or
    /// waits (spec §13.4).
    fn maybe_start(&mut self, id: &str) {
        let Some(rt) = self.models.get(id) else {
            return;
        };
        if rt.state != ModelState::Unloaded {
            return;
        }
        if let Some(rejection) = self.gate_block(id) {
            self.reject_waiters(id, &rejection);
            if let Some(rt) = self.models.get_mut(id) {
                rt.reload_after_stop = false;
            }
            return;
        }

        self.refresh_footprint(id);
        // Implements FR-VRAM-7.
        let margin = u64::from(self.cfg.vram.make_room.margin_mib);
        let need = self.models.get(id).and_then(|rt| rt.footprint_mib);
        if let (true, Some(sample), Some(need)) = (self.cfg.vram.make_room.enabled, self.last_sample.clone(), need) {
            let required = need + margin;
            let free = sample.free_mib();
            if free < required {
                let others_unloading = self
                    .models
                    .iter()
                    .any(|(k, m)| k != id && matches!(m.state, ModelState::Draining(_) | ModelState::Stopping(_)));
                if others_unloading {
                    // A release is already underway; retry when it completes.
                    self.set_awaiting_room(id, true);
                    return;
                }
                let victims = pick_make_room_victims(self.idle_candidates(id), free, required);
                if !victims.is_empty() {
                    self.set_awaiting_room(id, true);
                    for v in victims {
                        self.begin_unload(&v, UnloadCause::MakeRoom);
                    }
                    return;
                }
                // Step 4 of spec §13.4: nothing to evict, start anyway; the
                // overflow rule handles whatever llama.cpp's auto-fit does.
            }
        }
        self.set_awaiting_room(id, false);
        self.start(id);
    }

    /// Marks whether `id` waits for evictions.
    fn set_awaiting_room(&mut self, id: &str, value: bool) {
        if let Some(rt) = self.models.get_mut(id) {
            rt.awaiting_room = value;
        }
    }

    /// Ready, idle models other than `except` that could be evicted.
    fn idle_candidates(&self, except: &str) -> Vec<Candidate> {
        self.models
            .iter()
            .filter(|(k, m)| k.as_str() != except && m.state == ModelState::Ready && m.inflight.is_empty())
            .map(|(k, m)| self.candidate(k, m))
            .collect()
    }

    /// Eviction candidate record for model `id`.
    fn candidate(&self, id: &str, rt: &ModelRt) -> Candidate {
        Candidate {
            id: id.to_owned(),
            pinned: self.model_cfg(id).is_some_and(|c| c.lifecycle.pinned),
            last_used: rt.last_used,
            footprint_mib: rt.footprint_mib.unwrap_or(0),
        }
    }

    /// Forgets records of models that were removed from the configuration and
    /// are now fully unloaded, so they stop appearing in status output.
    fn prune_removed(&mut self) {
        let removed: Vec<String> = self
            .models
            .iter()
            .filter(|(id, rt)| {
                !self.cfg.models.iter().any(|m| &m.id == *id)
                    && matches!(rt.state, ModelState::Unloaded | ModelState::Failed { .. } | ModelState::Held)
                    && rt.waiters.is_empty()
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in removed {
            self.models.remove(&id);
        }
    }

    /// Restarts models that were waiting for evictions to finish.
    fn retry_awaiting_room(&mut self) {
        let ids: Vec<String> = self
            .models
            .iter()
            .filter(|(_, m)| m.awaiting_room && m.state == ModelState::Unloaded)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            self.maybe_start(&id);
        }
    }

    /// Loads the learned footprint, or an estimate, into the record.
    fn refresh_footprint(&mut self, id: &str) {
        let Some(mc) = self.model_cfg(id) else { return };
        let hash = memory_args_hash(mc);
        let learned = self.deps.state.footprint(id, &hash);
        let known = learned.map(|mib| (mib, false)).or_else(|| estimate_for(mc).map(|mib| (mib, true)));
        if let Some(rt) = self.models.get_mut(id) {
            rt.args_hash = hash;
            if let Some((mib, estimated)) = known {
                rt.footprint_mib = Some(mib);
                rt.footprint_estimated = estimated;
            }
        }
    }

    /// Spawns the child and moves to `Starting` (spec §9.1).
    fn start(&mut self, id: &str) {
        let Some(mc) = self.model_cfg(id).cloned() else {
            return;
        };
        let generation = self.next_id();
        let stop = CancellationToken::new();
        self.deps.logs.clear(id);
        if let Some(rt) = self.models.get_mut(id) {
            rt.run = Some(Run {
                generation,
                pid: None,
                upstream: None,
                killer: None,
                stop_start_task: stop.clone(),
                kill_when_spawned: false,
            });
            rt.layers = None;
            rt.overflow = None;
            rt.baseline_shared_mib = None;
            rt.ready_at = None;
            rt.spill = super::vram_rules::SpillDetector::default();
            rt.launched_fingerprint = Some(fingerprint(&mc, &self.cfg.engine));
            rt.needs_restart = false;
            rt.reload_after_stop = false;
        }
        self.set_state(id, ModelState::Starting);

        let task = StartTask {
            model: mc,
            generation,
            launcher: self.deps.launcher.clone(),
            probe: self.deps.probe.clone(),
            events: self.deps.events.clone(),
            logs: self.deps.logs.clone(),
            tx: self.tx.clone(),
            poll: Duration::from_millis(u64::from(self.cfg.advanced.health_poll_interval_ms)),
            stop,
        };
        tokio::spawn(task.run());
    }

    /// Starts every model selected by `pick` that the gate allows.
    fn preload(&mut self, pick: impl Fn(&ModelConfig) -> bool) {
        let ids: Vec<String> = self.cfg.models.iter().filter(|m| m.enabled && pick(m)).map(|m| m.id.clone()).collect();
        for id in ids {
            if self.models.get(&id).is_some_and(|rt| rt.state == ModelState::Unloaded) {
                self.maybe_start(&id);
            }
        }
    }

    /// Explicit load request (button / admin API).
    fn load(&mut self, id: &str) -> Result<(), Rejection> {
        let canonical =
            self.cfg.find_model(id).map(|m| m.id.clone()).ok_or_else(|| {
                Rejection::new(RejectKind::UnknownModel, id, format!("No enabled model named '{id}'."))
            })?;
        if let Some(rejection) = self.gate_block(&canonical) {
            return Err(rejection);
        }
        match self.models.get(&canonical).map(|rt| rt.state.clone()) {
            Some(ModelState::Held) => Err(self.guard_rejection(&canonical)),
            Some(ModelState::Unloaded | ModelState::Failed { .. }) => {
                self.set_state(&canonical, ModelState::Unloaded);
                if let Some(rt) = self.models.get_mut(&canonical) {
                    rt.reload_after_stop = true;
                }
                self.maybe_start(&canonical);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Tray/GUI "Unload" (with optional hold).
    fn user_unload(&mut self, id: &str, hold: bool) {
        let Some(canonical) = self
            .cfg
            .find_model(id)
            .map(|m| m.id.clone())
            .or_else(|| self.models.contains_key(id).then(|| id.to_owned()))
        else {
            return;
        };
        if let Some(rt) = self.models.get_mut(&canonical) {
            rt.manual_hold = hold;
            rt.reload_after_stop = false;
        }
        self.begin_unload(&canonical, UnloadCause::User);
    }

    /// Work after a wake from sleep: drivers sometimes reset, so every loaded
    /// model must still answer `/health`. Slow answers get a few tries.
    fn after_resume(&mut self) {
        self.publish_gate();
        for (id, rt) in &self.models {
            if rt.state != ModelState::Ready {
                continue;
            }
            let Some(run) = rt.run.as_ref() else { continue };
            let Some(upstream) = run.upstream.clone() else { continue };
            let (probe, tx, model, generation) = (self.deps.probe.clone(), self.tx.clone(), id.clone(), run.generation);
            tokio::spawn(async move {
                for attempt in 0..RESUME_HEALTH_ATTEMPTS {
                    if probe.check(&upstream).await == crate::ports::Health::Ready {
                        return;
                    }
                    if attempt + 1 < RESUME_HEALTH_ATTEMPTS {
                        tokio::time::sleep(RESUME_HEALTH_PAUSE).await;
                    }
                }
                let _ = tx.send(Cmd::Unhealthy { model, generation });
            });
        }
    }

    /// Restart-now button: unload (per drain policy) then load again.
    fn restart(&mut self, id: &str) {
        let Some(canonical) = self.cfg.find_model(id).map(|m| m.id.clone()) else {
            return;
        };
        let state = self.models.get(&canonical).map(|rt| rt.state.clone());
        match state {
            Some(ModelState::Ready | ModelState::Starting) => {
                if let Some(rt) = self.models.get_mut(&canonical) {
                    rt.reload_after_stop = true;
                }
                self.begin_unload(&canonical, UnloadCause::ConfigChange);
            }
            Some(ModelState::Unloaded) => self.maybe_start(&canonical),
            _ => {}
        }
    }

    // ------------------------------------------------------------ unloading

    /// Starts unloading `id` for `cause` (spec §7.2 triggers).
    fn begin_unload(&mut self, id: &str, cause: UnloadCause) {
        let Some(rt) = self.models.get(id) else {
            return;
        };
        let state = rt.state.clone();
        let has_waiters = !rt.waiters.is_empty();

        // Causes that reject new requests also reject anything already queued.
        if cause.rejects_new_requests() && has_waiters {
            let rejection = self.cause_rejection(id, cause);
            self.reject_waiters(id, &rejection);
        }

        match state {
            ModelState::Unloaded | ModelState::Failed { .. } | ModelState::Held | ModelState::Stopping(_) => {}
            ModelState::Starting => {
                self.note_unload(id, cause);
                if cause == UnloadCause::Overflow {
                    // The triggering request gets the error; the next request
                    // reloads and the loop guard caps repeats (spec §13.2).
                    let rejection = Rejection::new(
                        RejectKind::RequestCancelled,
                        id,
                        format!("Model '{id}' overflowed into system RAM while loading and was unloaded."),
                    )
                    .with_detail(json!({ "cause": "overflow" }));
                    self.reject_waiters(id, &rejection);
                }
                // Implements the "game starts while Starting" row of spec §21.
                self.enter_stopping(id, cause);
            }
            ModelState::Ready => {
                self.note_unload(id, cause);
                self.set_state(id, ModelState::Draining(cause));
                self.apply_drain(id, cause);
            }
            ModelState::Draining(current) => {
                // An overflow error overrides a patient drain: the user chose
                // "error", so waiting makes no sense.
                if cause == UnloadCause::Overflow && current != UnloadCause::Overflow {
                    self.set_state(id, ModelState::Draining(cause));
                    self.apply_drain(id, cause);
                }
            }
        }
    }

    /// Records the unload for the loop guard when the cause counts.
    fn note_unload(&mut self, id: &str, cause: UnloadCause) {
        if !cause.counts_for_loop_guard() {
            return;
        }
        let window = loop_guard_window(&self.cfg);
        if let Some(rt) = self.models.get_mut(id) {
            rt.loop_guard.record(Instant::now(), window);
        }
    }

    /// Executes the drain plan for a model that just entered `Draining`.
    fn apply_drain(&mut self, id: &str, cause: UnloadCause) {
        let inflight_empty = self.models.get(id).is_none_or(|rt| rt.inflight.is_empty());
        if inflight_empty {
            self.enter_stopping(id, cause);
            return;
        }
        match drain::plan(cause, &self.cfg.draining) {
            DrainPlan::WaitForever => {}
            DrainPlan::WaitThenCancel(wait) => {
                let timer = self.next_id();
                if let Some(rt) = self.models.get_mut(id) {
                    rt.drain_timer = timer;
                }
                self.send_after(wait, Cmd::DrainTimeout { model: id.to_owned(), timer });
            }
            DrainPlan::CancelNow => {
                self.cancel_inflight(id, cause);
                self.enter_stopping(id, cause);
            }
        }
    }

    /// The drain wait elapsed.
    fn on_drain_timeout(&mut self, id: &str, timer: u64) {
        let Some(rt) = self.models.get(id) else {
            return;
        };
        if let ModelState::Draining(cause) = rt.state
            && rt.drain_timer == timer
        {
            self.cancel_inflight(id, cause);
            self.enter_stopping(id, cause);
        }
    }

    /// Cancels every in-flight request of `id` with a reason (spec §7.4).
    fn cancel_inflight(&mut self, id: &str, cause: UnloadCause) {
        let message = self.cancel_message(id, cause);
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        for (_, handle) in rt.inflight.drain() {
            handle.cancel(CancelInfo { cause: CancelCause::Unload(cause), message: message.clone() });
        }
    }

    /// Sentence telling a client why its request was cancelled.
    fn cancel_message(&self, id: &str, cause: UnloadCause) -> String {
        match cause {
            UnloadCause::Game => {
                let name = self.gate.current_game().map_or("a game", |(n, _)| n);
                format!("Model '{id}' was unloaded because a game started ({name}).")
            }
            UnloadCause::Pause => {
                format!("Model '{id}' was unloaded because the gatekeeper was paused.")
            }
            UnloadCause::VramPressure => {
                format!("Model '{id}' was unloaded because GPU memory was running low.")
            }
            UnloadCause::Overflow => {
                format!("Model '{id}' overflowed into system RAM and was unloaded.")
            }
            UnloadCause::IdleTtl => format!("Model '{id}' was unloaded after being idle."),
            UnloadCause::MakeRoom => {
                format!("Model '{id}' was unloaded to make room for another model.")
            }
            UnloadCause::User => format!("Model '{id}' was unloaded by the user."),
            UnloadCause::ConfigChange => {
                format!("Model '{id}' is restarting because its settings changed.")
            }
            UnloadCause::Shutdown => "The gatekeeper is shutting down.".to_owned(),
        }
    }

    /// Kills the process and moves to `Stopping`.
    fn enter_stopping(&mut self, id: &str, cause: UnloadCause) {
        let baseline = self.last_sample.as_ref().map(|s| s.used_mib);
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        // Only wait for VRAM release if the model had time to allocate.
        rt.release_baseline_used_mib = if rt.ready_at.is_some() { baseline } else { None };
        let mut no_process = false;
        if let Some(run) = rt.run.as_mut() {
            run.stop_start_task.cancel();
            match &run.killer {
                Some(k) => k.kill(),
                // Not spawned yet: kill the moment it appears.
                None => run.kill_when_spawned = true,
            }
        } else {
            no_process = true;
        }
        self.set_state(id, ModelState::Stopping(cause));
        if no_process {
            self.complete_stop(id);
        }
    }

    /// The process exited while stopping; wait for the driver to free VRAM
    /// (spec §9.2) before reporting `Unloaded`.
    fn begin_release_wait(&mut self, id: &str) {
        let wait = Duration::from_millis(u64::from(self.cfg.advanced.stop_release_wait_ms));
        let can_wait =
            self.models.get(id).is_some_and(|rt| rt.footprint_mib.is_some() && rt.release_baseline_used_mib.is_some())
                && self.last_sample.is_some();
        if !can_wait {
            self.complete_stop(id);
            return;
        }
        let timer = self.next_id();
        if let Some(rt) = self.models.get_mut(id) {
            rt.release_timer = timer;
        }
        self.send_after(wait, Cmd::ReleaseTimeout { model: id.to_owned(), timer });
    }

    /// The release wait elapsed without seeing the drop.
    fn on_release_timeout(&mut self, id: &str, timer: u64) {
        if self
            .models
            .get(id)
            .is_some_and(|rt| rt.release_timer == timer && matches!(rt.state, ModelState::Stopping(_)))
        {
            debug!(model = id, "VRAM release not observed in time; continuing");
            self.complete_stop(id);
        }
    }

    /// Finishes an unload: `Stopping -> Unloaded` (or `Held`) and reloads
    /// for queued requests (FR-VRAM-3).
    fn complete_stop(&mut self, id: &str) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        let cause = rt.state.cause();
        rt.run = None;
        rt.inflight.clear();
        rt.ready_at = None;
        rt.release_timer = 0;
        rt.release_baseline_used_mib = None;
        rt.layers = None;

        let window = loop_guard_window(&self.cfg);
        let max = self.cfg.vram.loop_guard.max_unloads;
        let guard_on = self.model_cfg(id).map_or(self.cfg.vram.loop_guard.enabled, |m| {
            m.vram_overrides.loop_guard_enabled.unwrap_or(self.cfg.vram.loop_guard.enabled)
        });
        let tripped = guard_on
            && cause.is_some_and(UnloadCause::counts_for_loop_guard)
            && self.models.get_mut(id).is_some_and(|rt| rt.loop_guard.tripped(Instant::now(), window, max));

        if tripped {
            // Implements FR-VRAM-6: hold instead of reloading.
            self.set_state(id, ModelState::Held);
            if let Some(rt) = self.models.get_mut(id) {
                rt.held_ok_since = None;
                rt.reload_after_stop = false;
            }
            let rejection = self.guard_rejection(id);
            self.reject_waiters(id, &rejection);
            self.notify(
                NotificationKind::LoopGuard,
                "Model held",
                format!(
                    "'{id}' was unloaded {max} times in {} min and is held until VRAM frees up.",
                    window.as_secs() / 60
                ),
            );
        } else {
            self.set_state(id, ModelState::Unloaded);
            let reload = self.models.get(id).is_some_and(|rt| !rt.waiters.is_empty() || rt.reload_after_stop);
            if reload {
                self.maybe_start(id);
            }
        }
        self.prune_removed();
        self.retry_awaiting_room();
    }

    /// Releases a held model (button or free VRAM).
    fn release_hold(&mut self, id: &str) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        rt.manual_hold = false;
        if rt.state == ModelState::Held {
            rt.loop_guard.clear();
            rt.held_ok_since = None;
            self.set_state(id, ModelState::Unloaded);
        }
    }

    // ----------------------------------------------------------- process events

    /// Handles events from start/exit tasks, ignoring stale generations.
    fn on_model_event(&mut self, ev: ModelEvent) {
        match ev {
            ModelEvent::Spawned { model, generation, pid, upstream, killer } => {
                let Some(run) =
                    self.models.get_mut(&model).and_then(|rt| rt.run.as_mut()).filter(|r| r.generation == generation)
                else {
                    // The run was abandoned while spawning: don't leak the child.
                    killer.kill();
                    return;
                };
                run.pid = Some(pid);
                run.upstream = Some(upstream);
                run.killer = Some(killer.clone());
                if run.kill_when_spawned {
                    killer.kill();
                }
            }
            ModelEvent::SpawnFailed { model, generation, error } => {
                if !self.is_current(&model, generation) {
                    return;
                }
                match self.models.get(&model).map(|rt| rt.state.clone()) {
                    Some(ModelState::Starting) => self.fail_start(&model, error),
                    Some(ModelState::Stopping(_)) => self.complete_stop(&model),
                    _ => {}
                }
            }
            ModelEvent::Ready { model, generation } => {
                if self.is_current_in(&model, generation, &ModelState::Starting) {
                    if let Some(rt) = self.models.get_mut(&model) {
                        rt.ready_at = Some(Instant::now());
                        rt.fail_count = 0;
                        rt.port_retries = 0;
                    }
                    self.set_state(&model, ModelState::Ready);
                    self.resolve_waiters(&model);
                }
            }
            ModelEvent::StartTimedOut { model, generation } => {
                if self.is_current_in(&model, generation, &ModelState::Starting) {
                    self.fail_start(&model, "the model did not become ready in time".to_owned());
                }
            }
            ModelEvent::Exited { model, generation, info } => self.on_exited(&model, generation, &info),
            ModelEvent::Layers { model, generation, offloaded, total } => {
                if !self.is_current(&model, generation) {
                    return;
                }
                if let Some(rt) = self.models.get_mut(&model) {
                    rt.layers = Some((offloaded, total));
                }
                let setting = self
                    .model_cfg(&model)
                    .and_then(|m| ["n-gpu-layers", "gpu-layers", "ngl"].iter().find_map(|k| m.llama_args.get(*k)))
                    .cloned();
                // Implements FR-VRAM-4 (check 1, spec §13.2).
                if self.cfg.vram.overflow.check_layer_offload && layers_overflow(offloaded, total, setting.as_ref()) {
                    self.on_overflow(&model, format!("only {offloaded} of {total} layers were placed on the GPU"));
                }
            }
        }
    }

    /// True if `generation` is the model's current run.
    fn is_current(&self, id: &str, generation: u64) -> bool {
        self.models.get(id).and_then(|rt| rt.run.as_ref()).is_some_and(|r| r.generation == generation)
    }

    /// True if `generation` is the model's current run and it is in `state`.
    fn is_current_in(&self, id: &str, generation: u64, state: &ModelState) -> bool {
        self.is_current(id, generation) && self.models.get(id).is_some_and(|rt| rt.state == *state)
    }

    /// A child process ended.
    fn on_exited(&mut self, id: &str, generation: u64, info: &ExitInfo) {
        if !self.is_current(id, generation) {
            return;
        }
        let state = match self.models.get(id) {
            Some(rt) => rt.state.clone(),
            None => return,
        };
        match state {
            ModelState::Stopping(_) => self.begin_release_wait(id),
            ModelState::Draining(cause) => {
                // The child died while we waited for requests to finish.
                self.cancel_inflight_crashed(id);
                self.set_state(id, ModelState::Stopping(cause));
                self.complete_stop(id);
            }
            ModelState::Starting => {
                // Spec §21: a port taken between `free_port` and the child's own
                // bind is retried on a fresh port instead of backing off.
                let collided = info.log_tail.iter().any(|l| is_bind_failure(l));
                let retries = self.models.get(id).map_or(u32::MAX, |rt| rt.port_retries);
                if collided && retries < MAX_PORT_RETRIES {
                    warn!(model = id, attempt = retries + 1, "port collision while starting; retrying on a new port");
                    if let Some(rt) = self.models.get_mut(id) {
                        rt.port_retries += 1;
                    }
                    self.start(id);
                    return;
                }
                let tail =
                    info.log_tail.iter().rev().take(FAILURE_LOG_LINES).rev().cloned().collect::<Vec<_>>().join("\n");
                let message = format!("llama-server exited during startup (code {:?}). {tail}", info.code);
                self.fail_start(id, message);
            }
            ModelState::Ready => {
                // Implements the Crashed row of spec §7.2: waiters get
                // model_crashed, the next request starts it again.
                warn!(model = id, code = ?info.code, "model process exited unexpectedly");
                self.cancel_inflight_crashed(id);
                let rejection = Rejection::new(
                    RejectKind::ModelCrashed,
                    id,
                    format!("Model '{id}' crashed (exit code {:?}).", info.code),
                )
                .with_detail(json!({ "exit_code": info.code, "log_tail": info.log_tail }));
                self.reject_waiters(id, &rejection);
                if let Some(rt) = self.models.get_mut(id) {
                    rt.run = None;
                    rt.ready_at = None;
                }
                self.set_state(id, ModelState::Unloaded);
                self.notify(NotificationKind::LoadFailure, "Model crashed", format!("'{id}' exited unexpectedly."));
                self.retry_awaiting_room();
            }
            ModelState::Unloaded | ModelState::Held | ModelState::Failed { .. } => {}
        }
    }

    /// Cancels in-flight requests because the process died.
    fn cancel_inflight_crashed(&mut self, id: &str) {
        let message = format!("Model '{id}' crashed.");
        if let Some(rt) = self.models.get_mut(id) {
            for (_, h) in rt.inflight.drain() {
                h.cancel(CancelInfo { cause: CancelCause::Crash, message: message.clone() });
            }
        }
    }

    /// A start failed: back off and tell waiters (spec §9.3).
    fn fail_start(&mut self, id: &str, message: String) {
        let max = u64::from(self.cfg.advanced.load_failure_backoff_max_seconds);
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        rt.fail_count = rt.fail_count.saturating_add(1);
        let exp = rt.fail_count.saturating_sub(1).min(16);
        let backoff = BACKOFF_BASE_SECONDS.saturating_mul(1u64 << exp).min(max);
        if let Some(run) = rt.run.take() {
            run.abort();
        }
        rt.ready_at = None;
        self.set_state(
            id,
            ModelState::Failed { until: Instant::now() + Duration::from_secs(backoff), message: message.clone() },
        );
        let rejection =
            Rejection::new(RejectKind::ModelLoadFailed, id, format!("Model '{id}' failed to start: {message}"))
                .with_detail(json!({ "log_tail": self.deps.logs.tail(id, FAILURE_LOG_LINES) }))
                .with_retry_after(u32::try_from(backoff).unwrap_or(u32::MAX));
        self.reject_waiters(id, &rejection);
        self.notify(NotificationKind::LoadFailure, "Model failed to load", format!("'{id}': {message}"));
        self.retry_awaiting_room();
    }

    // --------------------------------------------------------------- games

    /// A detector reported.
    fn on_game(&mut self, signal: GameSignal) {
        if !self.cfg.games.enabled {
            return;
        }
        match self.gate.signal(&signal) {
            SignalEffect::None => {}
            SignalEffect::SessionStarted => {
                info!(game = ?signal.name, source = %signal.source, "game session started");
                self.publish_gate();
                let name = signal.name.clone().unwrap_or_else(|| "a game".to_owned());
                self.notify(NotificationKind::GameGate, "Game detected", format!("{name}: models unloading."));
                // Implements FR-GAME-2..4.
                let affected: Vec<String> = self
                    .cfg
                    .models
                    .iter()
                    .filter(|m| m.lifecycle.game_policy == GamePolicy::Unload)
                    .map(|m| m.id.clone())
                    .collect();
                for id in affected {
                    self.begin_unload(&id, UnloadCause::Game);
                }
            }
            SignalEffect::StartCooldown(id) => {
                let cooldown = Duration::from_secs(u64::from(self.cfg.games.cooldown_seconds));
                if cooldown.is_zero() {
                    self.on_cooldown_elapsed(id);
                } else {
                    self.send_after(cooldown, Cmd::CooldownElapsed { id });
                }
            }
        }
    }

    /// The cooldown ended: the game session is over (FR-GAME-6).
    fn on_cooldown_elapsed(&mut self, id: u64) {
        if self.gate.cooldown_elapsed(id) {
            info!("game session ended");
            self.publish_gate();
            self.notify(NotificationKind::GameGate, "Game closed", "Models are available again.".to_owned());
            self.preload(|m| m.lifecycle.preload_after_game);
        }
    }

    /// Pause or resume.
    fn set_paused(&mut self, paused: bool) {
        self.gate.paused = paused;
        self.publish_gate();
        if paused {
            for id in self.model_ids() {
                self.begin_unload(&id, UnloadCause::Pause);
            }
        }
    }

    // ---------------------------------------------------------------- vram

    /// New GPU reading: learn footprints, then apply every VRAM rule.
    fn on_vram(&mut self, sample: VramSample) {
        self.deps.events.publish(AppEvent::Vram(sample.clone()));
        self.last_sample = Some(sample.clone());
        self.update_baselines(&sample);
        self.check_spill(&sample);
        self.check_release_waits(&sample);
        self.check_pressure(&sample);
        self.check_held(&sample);
    }

    /// Reads the footprint and spill baseline once a model has settled
    /// (spec §9.5).
    fn update_baselines(&mut self, s: &VramSample) {
        let now = Instant::now();
        let mut learned: Vec<(String, String, u64)> = Vec::new();
        for (id, rt) in &mut self.models {
            if rt.state != ModelState::Ready || rt.baseline_shared_mib.is_some() {
                continue;
            }
            let (Some(ready_at), Some(pid)) = (rt.ready_at, rt.run.as_ref().and_then(|r| r.pid)) else {
                continue;
            };
            if now.duration_since(ready_at) < BASELINE_DELAY {
                continue;
            }
            let Some(p) = s.process(pid) else { continue };
            rt.baseline_shared_mib = Some(p.shared_mib);
            if p.dedicated_mib > 0 {
                rt.footprint_mib = Some(p.dedicated_mib);
                rt.footprint_estimated = false;
                learned.push((id.clone(), rt.args_hash.clone(), p.dedicated_mib));
            }
        }
        for (id, hash, mib) in learned {
            self.deps.state.set_footprint(&id, &hash, mib);
        }
    }

    /// Driver-spill check (spec §13.2 check 2).
    fn check_spill(&mut self, s: &VramSample) {
        if !self.cfg.vram.overflow.check_driver_spill {
            return;
        }
        let threshold = u64::from(self.cfg.vram.overflow.spill_threshold_mib);
        let mut spilled: Vec<(String, u64)> = Vec::new();
        for (id, rt) in &mut self.models {
            if rt.state != ModelState::Ready || rt.overflow.is_some() {
                continue;
            }
            let (Some(base), Some(pid)) = (rt.baseline_shared_mib, rt.run.as_ref().and_then(|r| r.pid)) else {
                continue;
            };
            let Some(p) = s.process(pid) else { continue };
            if rt.spill.observe(p.shared_mib, base, threshold) {
                spilled.push((id.clone(), p.shared_mib.saturating_sub(base)));
            }
        }
        for (id, grown) in spilled {
            self.on_overflow(&id, format!("the driver moved {grown} MiB of the model into system RAM"));
        }
    }

    /// Applies the configured overflow action (FR-VRAM-5).
    fn on_overflow(&mut self, id: &str, detail: String) {
        let Some(rt) = self.models.get_mut(id) else {
            return;
        };
        if rt.overflow.is_some() {
            return;
        }
        rt.overflow = Some(detail.clone());
        warn!(model = id, %detail, "model overflowed into system RAM");
        self.notify(NotificationKind::Overflow, "Model overflowed into RAM", format!("'{id}': {detail}."));
        let action =
            self.model_cfg(id).and_then(|m| m.vram_overrides.overflow_action).unwrap_or(self.cfg.vram.overflow.action);
        if action == OverflowAction::ErrorAndUnload {
            self.begin_unload(id, UnloadCause::Overflow);
        }
    }

    /// Completes release waits when adapter usage has dropped.
    fn check_release_waits(&mut self, s: &VramSample) {
        let done: Vec<String> = self
            .models
            .iter()
            .filter(|(_, rt)| matches!(rt.state, ModelState::Stopping(_)) && rt.release_timer != 0)
            .filter(|(_, rt)| match (rt.footprint_mib, rt.release_baseline_used_mib) {
                (Some(fp), Some(base)) => base.saturating_sub(s.used_mib) >= fp * RELEASE_FRACTION_PERCENT / 100,
                _ => true,
            })
            .map(|(k, _)| k.clone())
            .collect();
        for id in done {
            self.complete_stop(&id);
        }
    }

    /// Pressure rule (spec §13.1, FR-VRAM-2).
    fn check_pressure(&mut self, s: &VramSample) {
        let cfg = self.cfg.vram.pressure.clone();
        if !cfg.enabled {
            self.pressure.reset();
            return;
        }
        let above = pressure_percent(s.used_mib, s.total_mib) >= cfg.threshold_percent;
        let any_loaded = self.models.values().any(|m| matches!(m.state, ModelState::Starting | ModelState::Ready));
        let already_unloading = self.models.values().any(|m| m.state.cause() == Some(UnloadCause::VramPressure));
        if !above || !any_loaded || already_unloading {
            self.pressure.reset();
            return;
        }
        let sustain = Duration::from_secs(u64::from(cfg.sustain_seconds));
        if !self.pressure.observe(Instant::now(), true, sustain) {
            return;
        }
        let candidates: Vec<Candidate> = self
            .models
            .iter()
            .filter(|(_, m)| matches!(m.state, ModelState::Starting | ModelState::Ready))
            .map(|(k, m)| self.candidate(k, m))
            .collect();
        for victim in pick_pressure_victims(candidates, cfg.scope) {
            self.notify(
                NotificationKind::VramUnload,
                "GPU memory is low",
                format!("Unloading '{victim}' ({}% VRAM in use).", pressure_percent(s.used_mib, s.total_mib)),
            );
            self.begin_unload(&victim, UnloadCause::VramPressure);
        }
    }

    /// Releases held models once enough VRAM stays free (spec §13.3).
    fn check_held(&mut self, s: &VramSample) {
        let guard = self.cfg.vram.loop_guard.clone();
        let sustain = Duration::from_secs(u64::from(guard.release_sustain_seconds));
        let now = Instant::now();
        let mut release: Vec<String> = Vec::new();
        for (id, rt) in &mut self.models {
            if rt.state != ModelState::Held {
                continue;
            }
            let need = rt.footprint_mib.unwrap_or(0) + u64::from(guard.reload_margin_mib);
            if s.free_mib() >= need {
                let since = *rt.held_ok_since.get_or_insert(now);
                if now.duration_since(since) >= sustain {
                    release.push(id.clone());
                }
            } else {
                rt.held_ok_since = None;
            }
        }
        for id in release {
            self.release_hold(&id);
        }
    }

    // --------------------------------------------------------------- misc

    /// Periodic housekeeping: idle unloads (spec `idle_unload_minutes`).
    fn on_tick(&mut self) {
        let now = Instant::now();
        let idle: Vec<String> = self
            .models
            .iter()
            .filter(|(k, m)| {
                m.state == ModelState::Ready
                    && m.inflight.is_empty()
                    && self.model_cfg(k).is_some_and(|c| {
                        c.lifecycle.idle_unload_minutes > 0
                            && now.duration_since(m.last_used)
                                >= Duration::from_secs(u64::from(c.lifecycle.idle_unload_minutes) * 60)
                    })
            })
            .map(|(k, _)| k.clone())
            .collect();
        for id in idle {
            self.begin_unload(&id, UnloadCause::IdleTtl);
        }
    }

    /// Applies new settings (spec §14.5): flags models whose launch settings
    /// changed and unloads models that were removed or disabled.
    fn on_set_config(&mut self, cfg: Arc<Config>) {
        self.cfg = cfg;
        let now = Instant::now();
        for m in self.cfg.models.clone() {
            self.models.entry(m.id.clone()).or_insert_with(|| ModelRt::new(now));
            if let Some(rt) = self.models.get_mut(&m.id)
                && rt.state.has_process()
            {
                rt.needs_restart =
                    rt.launched_fingerprint.as_deref() != Some(fingerprint(&m, &self.cfg.engine).as_str());
            }
        }
        let gone: Vec<String> =
            self.models.keys().filter(|k| !self.cfg.models.iter().any(|m| &m.id == *k && m.enabled)).cloned().collect();
        for id in gone {
            let rejection =
                Rejection::new(RejectKind::UnknownModel, &id, format!("Model '{id}' is no longer available."));
            self.reject_waiters(&id, &rejection);
            self.begin_unload(&id, UnloadCause::ConfigChange);
        }
        self.prune_removed();
        self.deps.events.publish(AppEvent::ConfigReloaded);
    }

    /// Kills every child immediately; used when the app exits.
    fn shutdown(&mut self) {
        for id in self.model_ids() {
            let rejection = shutting_down(&id);
            self.reject_waiters(&id, &rejection);
            self.cancel_inflight(&id, UnloadCause::Shutdown);
            if let Some(run) = self.models.get_mut(&id).and_then(|rt| rt.run.take()) {
                run.abort();
            }
        }
    }

    /// Builds the status snapshot.
    fn status(&self) -> StatusSnapshot {
        let now = Instant::now();
        let mut models: Vec<ModelStatus> = self
            .models
            .iter()
            .map(|(id, rt)| ModelStatus {
                id: id.clone(),
                display_name: self.model_cfg(id).map_or_else(|| id.clone(), |m| m.display_name.clone()),
                state: rt.state.kind(),
                cause: rt.state.cause(),
                blocked_reason: self
                    .gate_block(id)
                    .map(|r| r.kind)
                    .or_else(|| (rt.state == ModelState::Held).then_some(RejectKind::VramGuard)),
                inflight: rt.inflight.len(),
                queued: rt.waiters.len(),
                footprint_mib: rt.footprint_mib,
                footprint_estimated: rt.footprint_estimated,
                gpu_layers: rt.layers,
                overflow: rt.overflow.clone(),
                needs_restart: rt.needs_restart,
                manual_hold: rt.manual_hold,
                pid: rt.run.as_ref().and_then(|r| r.pid),
                idle_seconds: now.duration_since(rt.last_used).as_secs(),
                failure: match &rt.state {
                    ModelState::Failed { message, .. } => Some(message.clone()),
                    _ => None,
                },
            })
            .collect();
        models.sort_by(|a, b| a.id.cmp(&b.id));
        let gate = GateIndicator::derive(self.gate.paused, self.gate.game_blocks(), models.iter().map(|m| m.state));
        StatusSnapshot {
            gate,
            paused: self.gate.paused,
            game: self.gate.active_game().map(|(n, _)| n.to_owned()),
            game_source: self.gate.active_game().map(|(_, s)| s.to_owned()),
            session_active: self.gate.session_active(),
            override_active: self.gate.override_active(),
            cooling_down: self.gate.cooling_down(),
            models,
            vram: self.last_sample.clone(),
        }
    }
}

/// Health checks tried after a resume before a model is restarted.
const RESUME_HEALTH_ATTEMPTS: u32 = 3;

/// Pause between those checks (the GPU and network stack may still be waking).
const RESUME_HEALTH_PAUSE: Duration = Duration::from_secs(3);

/// Fingerprint of the settings that require a restart when changed: the
/// model's own launch settings plus the engine build and device it runs on
/// (switching engines must not leave a loaded model on the old binary unnoticed).
fn fingerprint(m: &ModelConfig, engine: &crate::config::schema::EngineConfig) -> String {
    serde_json::to_string(&(&m.files, &m.llama_args, &m.extra_args, &m.env, &engine.llama_server_dir, &engine.device))
        .unwrap_or_default()
}

// ------------------------------------------------------------- start task

/// Everything the background start task needs; owns no controller state.
struct StartTask {
    /// Model to launch.
    model: ModelConfig,
    /// Launch generation for stale-event filtering.
    generation: u64,
    /// Process launcher.
    launcher: Arc<dyn ProcessLauncher>,
    /// Readiness probe.
    probe: Arc<dyn HealthProbe>,
    /// Event bus for log lines.
    events: EventBus,
    /// Log ring buffers.
    logs: LogStore,
    /// Channel back to the controller.
    tx: mpsc::UnboundedSender<Cmd>,
    /// Health poll interval.
    poll: Duration,
    /// Cancelled when the controller stops caring about this start.
    stop: CancellationToken,
}

impl StartTask {
    /// Launches, then polls `/health` until ready, timed out or cancelled.
    async fn run(self) {
        let id = self.model.id.clone();
        let generation = self.generation;
        let send = |ev: ModelEvent| {
            let _ = self.tx.send(Cmd::Model(ev));
        };

        let LaunchedProcess { pid, upstream, exited, killer, mut logs } = match self.launcher.launch(&self.model).await
        {
            Ok(p) => p,
            Err(e) => {
                send(ModelEvent::SpawnFailed { model: id, generation, error: e.0 });
                return;
            }
        };
        send(ModelEvent::Spawned { model: id.clone(), generation, pid, upstream: upstream.clone(), killer });

        // Pump log lines into the ring buffer, event bus and layer parser.
        {
            let (id, tx, store, events) = (id.clone(), self.tx.clone(), self.logs.clone(), self.events.clone());
            tokio::spawn(async move {
                while let Some(line) = logs.recv().await {
                    if let Some((offloaded, total)) = parse_layer_placement(&line) {
                        let _ =
                            tx.send(Cmd::Model(ModelEvent::Layers { model: id.clone(), generation, offloaded, total }));
                    }
                    events.publish(AppEvent::ModelLog { model: id.clone(), line: line.clone() });
                    store.push(&id, line);
                }
            });
        }
        // Report the exit exactly once, with recent log lines attached.
        {
            let (id, tx, store) = (id.clone(), self.tx.clone(), self.logs.clone());
            tokio::spawn(async move {
                let mut info = exited.await.unwrap_or_default();
                if info.log_tail.is_empty() {
                    info.log_tail = store.tail(&id, 200);
                }
                let _ = tx.send(Cmd::Model(ModelEvent::Exited { model: id, generation, info }));
            });
        }

        // A little longer than the requests' own wait, so a slow load is
        // reported to clients as `load_timeout` rather than a failed start.
        let deadline =
            Instant::now() + Duration::from_secs(u64::from(self.model.lifecycle.load_timeout_seconds)) + START_GRACE;
        loop {
            tokio::select! {
                () = self.stop.cancelled() => return,
                () = tokio::time::sleep(self.poll) => {}
            }
            if self.probe.check(&upstream).await == crate::ports::Health::Ready {
                send(ModelEvent::Ready { model: id, generation });
                return;
            }
            if Instant::now() >= deadline {
                send(ModelEvent::StartTimedOut { model: id, generation });
                return;
            }
        }
    }
}
