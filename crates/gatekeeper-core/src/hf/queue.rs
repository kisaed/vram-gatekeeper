//! The download queue (spec §15.3).
//!
//! Responsibility: run downloads up to `max_concurrent` at a time, with pause,
//! resume, cancel, a bandwidth limit, automatic retry of connection problems,
//! optional pausing while a game runs, and progress events for the UI.
//! Not responsible for the transfer itself (`download`) or for what a finished
//! download becomes (`library`).
//!
//! The queue is one actor, like the gate controller: every state change
//! happens in a single task, one message at a time.
//!
//! Implements FR-MOD-4, FR-GAME-6 (downloads may pause while gaming).

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::api::{HfApi, RepoFile};
use super::download::{Context, DownloadError, FileJob, RateLimiter, download_file, part_path, safe_dest};
use crate::config::schema::Config;
use crate::events::{AppEvent, EventBus, NotificationKind};
use crate::sync::MutexExt;

/// Progress events per second per download, at most.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Attempts at one file when the connection keeps failing.
const NETWORK_ATTEMPTS: u32 = 6;

/// Longest wait between connection retries.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(30);

/// What to download.
#[derive(Debug, Clone, Deserialize)]
pub struct DownloadSpec {
    /// Repository, `author/name`.
    pub repo: String,
    /// Branch, tag or commit.
    pub revision: String,
    /// Commit the listing came from, recorded in the model's source.
    pub commit_sha: Option<String>,
    /// Files, first shard first.
    pub files: Vec<RepoFile>,
    /// Display name, e.g. `Qwen3 8B (Q4_K_M)`.
    pub label: String,
}

/// Where a download is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    /// Waiting for a free slot.
    Queued,
    /// Transferring.
    Downloading,
    /// Paused by the user.
    Paused,
    /// Paused because a game is running.
    PausedForGame,
    /// Finished and verified.
    Done,
    /// Stopped with an error; resuming retries.
    Failed,
    /// Cancelled by the user.
    Cancelled,
}

/// What the UI shows for one download.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadInfo {
    /// Queue id.
    pub id: u64,
    /// Display name.
    pub label: String,
    /// Repository.
    pub repo: String,
    /// Revision.
    pub revision: String,
    /// Commit the files came from.
    pub commit_sha: Option<String>,
    /// State.
    pub state: DownloadState,
    /// Bytes on disk across all files.
    pub bytes_done: u64,
    /// Total bytes.
    pub bytes_total: u64,
    /// Recent speed in bytes per second.
    pub speed_bps: u64,
    /// File being transferred.
    pub current_file: Option<String>,
    /// Problem, when failed.
    pub error: Option<String>,
    /// True when `error` is that the repository needs a licence and access token.
    pub needs_access: bool,
    /// Final paths of the files, once done (first shard first).
    pub paths: Vec<String>,
    /// Repository paths of the files being downloaded (first shard first).
    pub files: Vec<String>,
}

impl DownloadInfo {
    /// Rebuilds the spec (without sizes or checksums) for recording a model's
    /// source after the download finished.
    pub fn to_spec(&self) -> DownloadSpec {
        DownloadSpec {
            repo: self.repo.clone(),
            revision: self.revision.clone(),
            commit_sha: self.commit_sha.clone(),
            files: self.files.iter().map(|p| RepoFile { path: p.clone(), size: 0, sha256: None }).collect(),
            label: self.label.clone(),
        }
    }
}

/// A queue entry.
struct Item {
    /// Public view, updated in place.
    info: DownloadInfo,
    /// What to fetch.
    spec: DownloadSpec,
    /// Stops the running task.
    stop: Option<CancellationToken>,
    /// Why the task was stopped, so the result is handled correctly.
    stop_reason: Option<DownloadState>,
    /// Rolling speed estimate: (time, bytes) of the last sample.
    last_sample: Option<(Instant, u64)>,
    /// Last time a progress event went out.
    last_emit: Option<Instant>,
    /// Files whose checksum already passed this session; a resume only
    /// checks their size instead of reading tens of GB again.
    verified: Arc<Mutex<HashSet<PathBuf>>>,
}

/// Messages to the queue actor.
enum Cmd {
    /// Add a download.
    Enqueue { spec: DownloadSpec, reply: oneshot::Sender<Result<u64, String>> },
    /// Pause one.
    Pause(u64),
    /// Resume or retry one.
    Resume(u64),
    /// Cancel one and delete its partial files.
    Cancel(u64),
    /// Remove a finished, failed or cancelled entry from the list.
    Dismiss(u64),
    /// Snapshot of all downloads.
    List { reply: oneshot::Sender<Vec<DownloadInfo>> },
    /// A game started (`true`) or ended (`false`).
    Gaming(bool),
    /// Progress from a task.
    Progress { id: u64, bytes_done: u64, file: String },
    /// A task finished.
    TaskDone { id: u64, result: Result<Vec<PathBuf>, DownloadError> },
}

/// Handle to the queue.
#[derive(Clone)]
pub struct DownloadManager {
    /// Channel into the actor.
    tx: mpsc::UnboundedSender<Cmd>,
}

impl DownloadManager {
    /// Starts the queue actor.
    pub fn spawn(api: HfApi, config: watch::Receiver<Arc<Config>>, events: EventBus) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let actor = Actor {
            api,
            config,
            events,
            rx,
            tx: tx.clone(),
            items: BTreeMap::new(),
            next_id: 1,
            gaming: false,
            limiter: None,
        };
        tokio::spawn(actor.run());
        Self { tx }
    }

    /// Adds a download and returns its id.
    ///
    /// # Errors
    /// Returns a message if a file name is unsafe or the queue is gone.
    pub async fn enqueue(&self, spec: DownloadSpec) -> Result<u64, String> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Cmd::Enqueue { spec, reply }).map_err(|_| "the download queue has stopped".to_owned())?;
        rx.await.map_err(|_| "the download queue has stopped".to_owned())?
    }

    /// Pauses a download, keeping its partial files.
    pub fn pause(&self, id: u64) {
        let _ = self.tx.send(Cmd::Pause(id));
    }

    /// Resumes a paused or failed download.
    pub fn resume(&self, id: u64) {
        let _ = self.tx.send(Cmd::Resume(id));
    }

    /// Cancels a download and deletes its partial files.
    pub fn cancel(&self, id: u64) {
        let _ = self.tx.send(Cmd::Cancel(id));
    }

    /// Removes a finished entry from the list.
    pub fn dismiss(&self, id: u64) {
        let _ = self.tx.send(Cmd::Dismiss(id));
    }

    /// All downloads.
    pub async fn list(&self) -> Vec<DownloadInfo> {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Cmd::List { reply }).is_err() {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    /// Tells the queue whether a game is running.
    pub fn set_gaming(&self, gaming: bool) {
        let _ = self.tx.send(Cmd::Gaming(gaming));
    }
}

/// The actor.
struct Actor {
    /// Hub client (URLs, HTTP, token).
    api: HfApi,
    /// Live settings.
    config: watch::Receiver<Arc<Config>>,
    /// Event bus for progress.
    events: EventBus,
    /// Inbox.
    rx: mpsc::UnboundedReceiver<Cmd>,
    /// Own sender, for tasks.
    tx: mpsc::UnboundedSender<Cmd>,
    /// Downloads by id (ordered, so the oldest queued goes first).
    items: BTreeMap<u64, Item>,
    /// Next id.
    next_id: u64,
    /// A game is running.
    gaming: bool,
    /// The bandwidth limiter every running download shares, with the rate it was built for.
    limiter: Option<(u32, Arc<RateLimiter>)>,
}

/// Folder downloads go to (`downloads.models_dir`, else `%USERPROFILE%\Models\gguf`).
pub fn models_dir(cfg: &Config) -> PathBuf {
    if let Some(d) = cfg.downloads.models_dir.as_deref().filter(|d| !d.trim().is_empty()) {
        // Windows accepts both separators, but stored paths read better (and
        // compare equal) when they use one style.
        return PathBuf::from(if cfg!(windows) { d.replace('/', "\\") } else { d.to_owned() });
    }
    std::env::var_os("USERPROFILE")
        .map_or_else(|| PathBuf::from("Models"), |h| PathBuf::from(h).join("Models").join("gguf"))
}

impl Actor {
    async fn run(mut self) {
        while let Some(cmd) = self.rx.recv().await {
            match cmd {
                Cmd::Enqueue { spec, reply } => {
                    let _ = reply.send(self.enqueue(spec));
                }
                Cmd::Pause(id) => self.stop_item(id, DownloadState::Paused),
                Cmd::Resume(id) => self.resume(id),
                Cmd::Cancel(id) => self.cancel(id),
                Cmd::Dismiss(id) => {
                    if self.items.get(&id).is_some_and(|i| {
                        matches!(i.info.state, DownloadState::Done | DownloadState::Failed | DownloadState::Cancelled)
                    }) {
                        self.items.remove(&id);
                    }
                }
                Cmd::List { reply } => {
                    let _ = reply.send(self.items.values().map(|i| i.info.clone()).collect());
                }
                Cmd::Gaming(g) => self.on_gaming(g),
                Cmd::Progress { id, bytes_done, file } => self.on_progress(id, bytes_done, &file),
                Cmd::TaskDone { id, result } => self.on_done(id, result),
            }
            self.schedule();
        }
    }

    fn enqueue(&mut self, spec: DownloadSpec) -> Result<u64, String> {
        if spec.files.is_empty() {
            return Err("nothing to download".to_owned());
        }
        let root = models_dir(&self.config.borrow());
        for f in &spec.files {
            if safe_dest(&root, &spec.repo, &f.path).is_none() {
                return Err(format!("'{}' is not a safe file name", f.path));
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let total = spec.files.iter().map(|f| f.size).sum();
        let info = DownloadInfo {
            id,
            label: spec.label.clone(),
            repo: spec.repo.clone(),
            revision: spec.revision.clone(),
            commit_sha: spec.commit_sha.clone(),
            state: DownloadState::Queued,
            bytes_done: 0,
            bytes_total: total,
            speed_bps: 0,
            current_file: None,
            error: None,
            needs_access: false,
            paths: Vec::new(),
            files: spec.files.iter().map(|f| f.path.clone()).collect(),
        };
        self.items.insert(
            id,
            Item {
                info,
                spec,
                stop: None,
                stop_reason: None,
                last_sample: None,
                last_emit: None,
                verified: Arc::default(),
            },
        );
        self.emit(id);
        Ok(id)
    }

    /// The shared limiter for `mbps`, rebuilt when the configured rate changes.
    fn limiter_for(&mut self, mbps: Option<u32>) -> Option<Arc<RateLimiter>> {
        let mbps = mbps?;
        if let Some((rate, limiter)) = &self.limiter
            && *rate == mbps
        {
            return Some(limiter.clone());
        }
        let limiter = RateLimiter::new(mbps);
        self.limiter = Some((mbps, limiter.clone()));
        Some(limiter)
    }

    /// Deletes the `.part` files of download `id` in the background.
    fn discard_partials(&self, id: u64) {
        if let Some(item) = self.items.get(&id) {
            tokio::spawn(delete_partials(models_dir(&self.config.borrow()), item.spec.clone()));
        }
    }

    /// Publishes the current view of one download.
    fn emit(&mut self, id: u64) {
        if let Some(item) = self.items.get_mut(&id) {
            item.last_emit = Some(Instant::now());
            self.events.publish(AppEvent::Download(item.info.clone()));
        }
    }

    /// Stops a running download (or marks a queued one) with `reason`.
    fn stop_item(&mut self, id: u64, reason: DownloadState) {
        let Some(item) = self.items.get_mut(&id) else { return };
        match item.info.state {
            DownloadState::Downloading => {
                item.stop_reason = Some(reason);
                if let Some(t) = &item.stop {
                    t.cancel();
                }
            }
            DownloadState::Queued => {
                item.info.state = reason;
                self.emit(id);
            }
            _ => {}
        }
    }

    fn resume(&mut self, id: u64) {
        let Some(item) = self.items.get_mut(&id) else { return };
        if matches!(item.info.state, DownloadState::Paused | DownloadState::PausedForGame | DownloadState::Failed) {
            item.info.state = DownloadState::Queued;
            item.info.error = None;
            item.info.needs_access = false;
            self.emit(id);
        }
    }

    fn cancel(&mut self, id: u64) {
        let Some(state) = self.items.get(&id).map(|i| i.info.state) else { return };
        match state {
            // on_done finishes the job once the task has stopped.
            DownloadState::Downloading => self.stop_item(id, DownloadState::Cancelled),
            DownloadState::Done | DownloadState::Cancelled => {}
            _ => {
                if let Some(item) = self.items.get_mut(&id) {
                    item.info.state = DownloadState::Cancelled;
                }
                self.discard_partials(id);
                self.emit(id);
            }
        }
    }

    fn on_gaming(&mut self, gaming: bool) {
        self.gaming = gaming;
        if !self.config.borrow().downloads.pause_while_gaming {
            return;
        }
        let ids: Vec<u64> = self.items.keys().copied().collect();
        for id in ids {
            let state = self.items.get(&id).map(|i| i.info.state);
            if gaming && matches!(state, Some(DownloadState::Downloading | DownloadState::Queued)) {
                self.stop_item(id, DownloadState::PausedForGame);
            } else if !gaming && state == Some(DownloadState::PausedForGame) {
                self.resume(id);
            }
        }
    }

    fn on_progress(&mut self, id: u64, bytes_done: u64, file: &str) {
        let Some(item) = self.items.get_mut(&id) else { return };
        let now = Instant::now();
        if let Some((t, b)) = item.last_sample {
            let dt = now.duration_since(t).as_secs_f64();
            if dt >= 0.5 {
                let rate = (bytes_done.saturating_sub(b)) as f64 / dt;
                // Smooth so the number does not jump around.
                item.info.speed_bps = ((item.info.speed_bps as f64) * 0.6 + rate * 0.4) as u64;
                item.last_sample = Some((now, bytes_done));
            }
        } else {
            item.last_sample = Some((now, bytes_done));
        }
        item.info.bytes_done = bytes_done;
        item.info.current_file = Some(file.to_owned());
        if item.last_emit.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL) {
            self.emit(id);
        }
    }

    fn on_done(&mut self, id: u64, result: Result<Vec<PathBuf>, DownloadError>) {
        let Some(item) = self.items.get_mut(&id) else { return };
        item.stop = None;
        item.info.speed_bps = 0;
        item.last_sample = None;
        let reason = item.stop_reason.take();
        match result {
            Ok(paths) => {
                item.info.state = DownloadState::Done;
                item.info.bytes_done = item.info.bytes_total;
                item.info.paths = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                item.info.current_file = None;
                info!(id, label = %item.info.label, "download finished");
                self.events.publish(AppEvent::Notification {
                    kind: NotificationKind::Download,
                    title: "Download finished".to_owned(),
                    body: item.info.label.clone(),
                });
            }
            Err(DownloadError::Stopped) => {
                item.info.state = reason.unwrap_or(DownloadState::Paused);
                if item.info.state == DownloadState::Cancelled {
                    self.discard_partials(id);
                }
            }
            Err(e) => {
                warn!(id, error = %e, "download failed");
                item.info.state = DownloadState::Failed;
                item.info.error = Some(e.to_string());
                item.info.needs_access = e == DownloadError::NeedsAccess;
                self.events.publish(AppEvent::Notification {
                    kind: NotificationKind::Download,
                    title: "Download failed".to_owned(),
                    body: format!("{}: {e}", item.info.label),
                });
            }
        }
        self.emit(id);
    }

    /// Starts queued downloads while slots are free.
    fn schedule(&mut self) {
        let (max, gate_games) = {
            let cfg = self.config.borrow();
            (cfg.downloads.max_concurrent.max(1) as usize, cfg.downloads.pause_while_gaming)
        };
        if gate_games && self.gaming {
            return; // everything waits for the game to end
        }
        let running = self.items.values().filter(|i| i.info.state == DownloadState::Downloading).count();
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter(|(_, i)| i.info.state == DownloadState::Queued)
            .map(|(id, _)| *id)
            .take(max.saturating_sub(running))
            .collect();
        for id in ids {
            self.start(id);
        }
    }

    fn start(&mut self, id: u64) {
        let (cfg, token) = (self.config.borrow().clone(), self.api.token());
        let root = models_dir(&cfg);
        let http = self.api.http().clone();
        let limiter = self.limiter_for(cfg.downloads.bandwidth_limit_mbps);
        let Some(item) = self.items.get_mut(&id) else { return };
        let stop = CancellationToken::new();
        item.stop = Some(stop.clone());
        item.stop_reason = None;
        item.info.state = DownloadState::Downloading;
        item.info.error = None;
        item.info.needs_access = false;

        let spec = item.spec.clone();
        let api = self.api.clone();
        let verify = cfg.downloads.verify_sha256;
        let verified = item.verified.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let ctx = Context { http: &http, token, verify, limiter, stop };
            let result = run_item(&ctx, &api, &spec, &root, &verified, id, &tx).await;
            let _ = tx.send(Cmd::TaskDone { id, result });
        });
        self.emit(id);
    }
}

/// Downloads every file of an item in order, retrying connection problems.
async fn run_item(
    ctx: &Context<'_>,
    api: &HfApi,
    spec: &DownloadSpec,
    root: &std::path::Path,
    verified: &Mutex<HashSet<PathBuf>>,
    id: u64,
    tx: &mpsc::UnboundedSender<Cmd>,
) -> Result<Vec<PathBuf>, DownloadError> {
    let mut finished_bytes = 0u64;
    let mut paths = Vec::new();
    for f in &spec.files {
        let dest = safe_dest(root, &spec.repo, &f.path)
            .ok_or_else(|| DownloadError::Io(format!("unsafe file name '{}'", f.path)))?;
        let job = FileJob {
            url: api.resolve_url(&spec.repo, &spec.revision, &f.path),
            dest: dest.clone(),
            size: f.size,
            sha256: f.sha256.clone(),
            // A finished file that already passed its checksum is only size-checked.
            trust_existing: verified.lock_ok().contains(&dest),
        };
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut last_sent = Instant::now().checked_sub(PROGRESS_INTERVAL).unwrap_or_else(Instant::now);
            let name = f.path.clone();
            let result = download_file(ctx, &job, |done| {
                // Throttle messages; the actor also throttles events.
                if last_sent.elapsed() >= PROGRESS_INTERVAL / 2 {
                    last_sent = Instant::now();
                    let _ = tx.send(Cmd::Progress { id, bytes_done: finished_bytes + done, file: name.clone() });
                }
            })
            .await;
            match result {
                Ok(()) => break,
                Err(DownloadError::Network(msg)) if attempt < NETWORK_ATTEMPTS => {
                    warn!(file = %f.path, attempt, error = %msg, "connection problem; will resume");
                    let delay = Duration::from_secs(2u64.pow(attempt.min(5))).min(MAX_RETRY_DELAY);
                    tokio::select! {
                        () = tokio::time::sleep(delay) => {}
                        () = ctx.stop.cancelled() => return Err(DownloadError::Stopped),
                    }
                }
                Err(e) => return Err(e),
            }
        }
        finished_bytes += f.size;
        verified.lock_ok().insert(dest.clone());
        paths.push(dest);
    }
    Ok(paths)
}

/// Deletes the `.part` files of a cancelled download (and finished files are
/// left alone).
async fn delete_partials(root: PathBuf, spec: DownloadSpec) {
    for f in &spec.files {
        if let Some(dest) = safe_dest(&root, &spec.repo, &f.path) {
            let _ = tokio::fs::remove_file(part_path(&dest)).await;
        }
    }
}
