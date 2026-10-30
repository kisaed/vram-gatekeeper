//! Process-list game detector (spec §11.1).
//!
//! Responsibility: every `games.scan_interval_seconds` (and on demand),
//! compare running executable names with the user's game list and report
//! changes.
//! Not responsible for: listing processes (a [`ProcessLister`]) or acting on
//! the result (see `gate::controller`).
//!
//! Implements FR-GAME-1, FR-GAME-8.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{GameDetector, GameSignal};
use crate::config::schema::{Config, GamesConfig};
use crate::error::CoreResult;

/// Source name in signals.
pub const SOURCE: &str = "process_list";

/// Source of the running executable names (lowercase not required).
pub trait ProcessLister: Send + Sync {
    /// Names of running executables, e.g. `["explorer.exe", "game.exe"]`.
    fn running_names(&self) -> Vec<String>;
}

/// Lists processes through a Toolhelp snapshot.
#[cfg(windows)]
pub struct WindowsProcessLister;

#[cfg(windows)]
impl ProcessLister for WindowsProcessLister {
    fn running_names(&self) -> Vec<String> {
        crate::platform::windows::process_list::list_processes()
            .map(|v| v.into_iter().map(|p| p.name).collect())
            .unwrap_or_default()
    }
}

/// Returns the name of the first running game from `names`, comparing file
/// names case-insensitively.
pub fn find_running_in(names_to_match: &[String], running: &[String]) -> Option<String> {
    let running: HashSet<String> = running.iter().map(|n| n.to_ascii_lowercase()).collect();
    names_to_match.iter().find(|n| running.contains(&n.to_ascii_lowercase())).cloned()
}

/// Returns the name of the first enabled listed game that is running.
///
/// Matching is by file name, case-insensitive; entries disabled by the user
/// are skipped (spec §11.1). The path stored with an entry is deliberately
/// not used: checking it would need a process handle (NFR-4).
pub fn find_running_game(games: &GamesConfig, running: &[String]) -> Option<String> {
    let enabled: Vec<String> = games.executables.iter().filter(|e| e.enabled).map(|e| e.name.clone()).collect();
    find_running_in(&enabled, running)
}

/// How long the Windows game list is cached between reads (it changes only
/// when Windows learns a new game).
const EXTRA_NAMES_TTL: Duration = Duration::from_secs(60);

/// Supplies extra executable names to treat as games (Windows' own list).
pub type ExtraNames = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Detector that polls a [`ProcessLister`].
pub struct ProcessScanDetector {
    /// Optional source of extra game names, used when `detect_windows_game_list` is on.
    extra: Option<ExtraNames>,
    /// Where process names come from.
    lister: Arc<dyn ProcessLister>,
    /// Wakes the scan loop early, e.g. on a foreground-window change.
    scan_now: Arc<Notify>,
    /// Stops the background task.
    stop: CancellationToken,
}

impl ProcessScanDetector {
    /// Creates a detector reading from `lister`.
    pub fn new(lister: Arc<dyn ProcessLister>) -> Self {
        Self { extra: None, lister, scan_now: Arc::new(Notify::new()), stop: CancellationToken::new() }
    }

    /// Adds a source of extra game names (Windows' own game list), consulted
    /// when `games.detect_windows_game_list` is on.
    #[must_use]
    pub fn with_extra_names(mut self, extra: ExtraNames) -> Self {
        self.extra = Some(extra);
        self
    }

    /// Handle other detectors use to request an immediate scan.
    pub fn scan_trigger(&self) -> Arc<Notify> {
        self.scan_now.clone()
    }
}

impl GameDetector for ProcessScanDetector {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn start(
        &mut self,
        signals: mpsc::UnboundedSender<GameSignal>,
        config: watch::Receiver<Arc<Config>>,
    ) -> CoreResult<()> {
        let (lister, wake, stop, extra) =
            (self.lister.clone(), self.scan_now.clone(), self.stop.clone(), self.extra.clone());
        tokio::spawn(scan_loop(lister, wake, stop, signals, config, extra));
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.cancel();
    }
}

/// The detector's background loop.
async fn scan_loop(
    lister: Arc<dyn ProcessLister>,
    wake: Arc<Notify>,
    stop: CancellationToken,
    signals: mpsc::UnboundedSender<GameSignal>,
    config: watch::Receiver<Arc<Config>>,
    extra: Option<ExtraNames>,
) {
    // The game we last reported as running, so only changes are sent.
    let mut reported: Option<String> = None;
    // Cached extra names and when they were read.
    let mut extra_cache: (Vec<String>, Option<tokio::time::Instant>) = (Vec::new(), None);
    loop {
        let cfg = config.borrow().clone();
        let games = &cfg.games;
        let enabled = games.enabled && games.detect_process_list;

        let found = if enabled {
            // The process snapshot and the registry read are synchronous Win32
            // calls; keep them off the async workers.
            let scan = lister.clone();
            let running = tokio::task::spawn_blocking(move || scan.running_names()).await.unwrap_or_default();
            if games.detect_windows_game_list
                && let Some(source) = &extra
                && extra_cache.1.is_none_or(|t| t.elapsed() >= EXTRA_NAMES_TTL)
            {
                let source = source.clone();
                let names = tokio::task::spawn_blocking(move || source()).await.unwrap_or_default();
                extra_cache = (names, Some(tokio::time::Instant::now()));
            }
            find_running_game(games, &running).or_else(|| {
                if !games.detect_windows_game_list || extra.is_none() {
                    return None;
                }
                // Ignored programs never count, even if Windows lists them.
                let usable: Vec<String> = extra_cache
                    .0
                    .iter()
                    .filter(|n| !games.ignored_executables.iter().any(|i| i.eq_ignore_ascii_case(n)))
                    .cloned()
                    .collect();
                find_running_in(&usable, &running)
            })
        } else {
            None
        };
        if found != reported {
            let signal = match &found {
                Some(name) => GameSignal::active(SOURCE, name),
                None => GameSignal::inactive(SOURCE),
            };
            if signals.send(signal).is_err() {
                return; // the app is shutting down
            }
            reported = found;
        }

        // Interval (spec §11.1): short enough for NFR-2, long enough for NFR-1.
        let wait = Duration::from_secs(u64::from(games.scan_interval_seconds.max(1)));
        tokio::select! {
            () = stop.cancelled() => return,
            () = wake.notified() => {}
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::config::schema::GameExecutable;

    /// Lister whose output the test controls.
    struct Scripted(Mutex<Vec<String>>);

    impl ProcessLister for Scripted {
        fn running_names(&self) -> Vec<String> {
            self.0.lock().expect("lock").clone()
        }
    }

    fn games(names: &[&str]) -> GamesConfig {
        GamesConfig {
            executables: names
                .iter()
                .map(|n| GameExecutable { name: (*n).into(), path: None, enabled: true })
                .collect(),
            ..GamesConfig::default()
        }
    }

    #[test]
    fn matching_is_case_insensitive_by_file_name() {
        let g = games(&["EldenRing.exe"]);
        assert_eq!(
            find_running_game(&g, &["explorer.exe".into(), "ELDENRING.EXE".into()]).as_deref(),
            Some("EldenRing.exe")
        );
        assert_eq!(find_running_game(&g, &["explorer.exe".into()]), None);
    }

    #[test]
    fn disabled_entries_are_skipped() {
        let mut g = games(&["a.exe"]);
        g.executables[0].enabled = false;
        assert_eq!(find_running_game(&g, &["a.exe".into()]), None);
    }

    #[tokio::test(start_paused = true)]
    async fn reports_start_and_exit_edges_only() {
        let lister = Arc::new(Scripted(Mutex::new(vec!["explorer.exe".into()])));
        let mut cfg = Config::default();
        cfg.games = games(&["game.exe"]);
        let (_cfg_tx, cfg_rx) = watch::channel(Arc::new(cfg));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut det = ProcessScanDetector::new(lister.clone());
        det.start(tx, cfg_rx).expect("start");

        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(rx.try_recv().is_err(), "nothing reported while no game runs");

        lister.0.lock().expect("lock").push("Game.exe".into());
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(rx.try_recv().expect("start signal"), GameSignal::active(SOURCE, "game.exe"));
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(rx.try_recv().is_err(), "no repeat while it keeps running");

        lister.0.lock().expect("lock").retain(|n| n != "Game.exe");
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert_eq!(rx.try_recv().expect("exit signal"), GameSignal::inactive(SOURCE));
        det.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn windows_game_list_names_count_only_when_enabled_and_not_ignored() {
        let lister = Arc::new(Scripted(Mutex::new(vec!["Cyberpunk2077.exe".into(), "javaw.exe".into()])));
        let extra: ExtraNames = Arc::new(|| vec!["cyberpunk2077.exe".to_owned(), "javaw.exe".to_owned()]);

        let mut off = Config::default();
        off.games.enabled = true;
        let (_t1, rx_off) = watch::channel(Arc::new(off.clone()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut d = ProcessScanDetector::new(lister.clone()).with_extra_names(extra.clone());
        d.start(tx, rx_off).expect("start");
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(rx.try_recv().is_err(), "off by default");
        d.stop();

        let mut on = off;
        on.games.detect_windows_game_list = true;
        on.games.ignored_executables = vec!["javaw.exe".into()];
        let (_t2, rx_on) = watch::channel(Arc::new(on));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut d = ProcessScanDetector::new(lister).with_extra_names(extra);
        d.start(tx, rx_on).expect("start");
        tokio::time::sleep(Duration::from_secs(5)).await;
        let sig = rx.try_recv().expect("detected via the Windows list");
        assert_eq!(sig, GameSignal::active(SOURCE, "cyberpunk2077.exe"));
        d.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn scan_trigger_detects_immediately() {
        let lister = Arc::new(Scripted(Mutex::new(vec![])));
        let mut cfg = Config::default();
        cfg.games = games(&["game.exe"]);
        cfg.games.scan_interval_seconds = 3600;
        let (_cfg_tx, cfg_rx) = watch::channel(Arc::new(cfg));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut det = ProcessScanDetector::new(lister.clone());
        let trigger = det.scan_trigger();
        det.start(tx, cfg_rx).expect("start");
        tokio::time::sleep(Duration::from_millis(10)).await;

        lister.0.lock().expect("lock").push("game.exe".into());
        trigger.notify_one();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            matches!(rx.try_recv(), Ok(s) if s.active),
            "foreground-triggered scan reports within milliseconds (NFR-2)"
        );
        det.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_gating_reports_nothing() {
        let lister = Arc::new(Scripted(Mutex::new(vec!["game.exe".into()])));
        let mut cfg = Config::default();
        cfg.games = games(&["game.exe"]);
        cfg.games.enabled = false;
        let (_cfg_tx, cfg_rx) = watch::channel(Arc::new(cfg));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut det = ProcessScanDetector::new(lister);
        det.start(tx, cfg_rx).expect("start");
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert!(rx.try_recv().is_err());
        det.stop();
    }
}
