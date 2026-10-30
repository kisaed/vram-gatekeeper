//! Steam game detector (spec §11.3, F7).
//!
//! Responsibility: watch `HKCU\Software\Valve\Steam\RunningAppID`, which Steam
//! sets to the running game's app id (and to 0 when none), using a registry
//! change notification instead of polling.
//! Not responsible for: other launchers or non-Steam games.
//!
//! Implements FR-GAME-1.

use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, watch};

use super::{GameDetector, GameSignal};
use crate::config::schema::Config;
use crate::error::CoreResult;
use crate::platform::windows::registry::{KeyWatcher, RegistryKey, watch as watch_key};
use crate::sync::MutexExt;

/// Source name in signals.
pub const SOURCE: &str = "steam";

/// Registry path of Steam's per-user settings.
const STEAM_KEY: &str = r"Software\Valve\Steam";

/// Value Steam keeps current with the running game's app id.
const RUNNING_APP_ID: &str = "RunningAppID";

/// Turns Steam's current `RunningAppID` into a signal.
///
/// App ids in `ignored` (tools that run through Steam) count as no game.
/// `name_of` looks up a display name and is only called for real games.
pub fn steam_signal(app_id: u32, ignored: &[u32], name_of: impl FnOnce(u32) -> Option<String>) -> GameSignal {
    if app_id == 0 || ignored.contains(&app_id) {
        return GameSignal::inactive(SOURCE);
    }
    let name = name_of(app_id).unwrap_or_else(|| format!("Steam app {app_id}"));
    GameSignal::active(SOURCE, &name)
}

/// Detector backed by a registry watcher.
pub struct SteamDetector {
    /// Registry path to watch; overridable so tests can use a throwaway key.
    key_path: String,
    /// Running watcher, dropped on stop.
    watcher: Option<KeyWatcher>,
}

impl Default for SteamDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl SteamDetector {
    /// Watches Steam's real key.
    pub fn new() -> Self {
        Self::with_key(STEAM_KEY)
    }

    /// Watches `key_path` under `HKEY_CURRENT_USER` (used by tests).
    pub fn with_key(key_path: &str) -> Self {
        Self { key_path: key_path.to_owned(), watcher: None }
    }
}

impl GameDetector for SteamDetector {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn start(
        &mut self,
        signals: mpsc::UnboundedSender<GameSignal>,
        config: watch::Receiver<Arc<Config>>,
    ) -> CoreResult<()> {
        let path = self.key_path.clone();
        // What we last reported, so only changes are sent.
        let last: Arc<Mutex<Option<GameSignal>>> = Arc::new(Mutex::new(None));
        self.watcher = Some(watch_key(&self.key_path, move || {
            let cfg = config.borrow().clone();
            let signal = if !cfg.games.enabled || !cfg.games.detect_steam {
                GameSignal::inactive(SOURCE)
            } else {
                let app_id = RegistryKey::open(&path).ok().and_then(|k| k.read_dword(RUNNING_APP_ID)).unwrap_or(0);
                steam_signal(app_id, &cfg.games.steam_ignored_app_ids, |id| {
                    RegistryKey::open(&format!(r"{path}\Apps\{id}")).ok().and_then(|k| k.read_string("Name"))
                })
            };
            let mut last = last.lock_ok();
            // Never report "inactive" before anything was active.
            let changed = match (&*last, signal.active) {
                (None, false) => false,
                (Some(prev), _) => *prev != signal,
                (None, true) => true,
            };
            if changed {
                let _ = signals.send(signal.clone());
                *last = Some(signal);
            }
        })?);
        Ok(())
    }

    fn stop(&mut self) {
        self.watcher = None; // dropping stops and joins the thread
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn zero_and_ignored_ids_are_not_games() {
        assert_eq!(steam_signal(0, &[], |_| None), GameSignal::inactive(SOURCE));
        assert_eq!(steam_signal(228_980, &[228_980], |_| None), GameSignal::inactive(SOURCE));
    }

    #[test]
    fn a_running_app_is_named_from_the_registry_or_by_id() {
        let named = steam_signal(1_245_620, &[], |_| Some("ELDEN RING".into()));
        assert_eq!(named, GameSignal::active(SOURCE, "ELDEN RING"));
        let anonymous = steam_signal(42, &[], |_| None);
        assert_eq!(anonymous, GameSignal::active(SOURCE, "Steam app 42"));
    }

    /// Uses a throwaway key, never Steam's real one.
    #[tokio::test]
    async fn registry_changes_become_signals_without_polling() {
        let base = format!(r"Software\VRAMGatekeeperTests\steam-{}", std::process::id());
        crate::platform::windows::registry::delete_tree(&base);
        let key = RegistryKey::create(&base).expect("create test key");
        key.set_dword(RUNNING_APP_ID, 0).expect("set");
        RegistryKey::create(&format!(r"{base}\Apps\730")).expect("app key");

        let (_cfg_tx, cfg_rx) = watch::channel(Arc::new(Config::default()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut det = SteamDetector::with_key(&base);
        det.start(tx, cfg_rx).expect("start");

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(rx.try_recv().is_err(), "no game running: nothing reported");

        key.set_dword(RUNNING_APP_ID, 730).expect("game starts");
        let started =
            tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("signal in time").expect("open");
        assert!(started.active);
        assert_eq!(started.source, SOURCE);

        key.set_dword(RUNNING_APP_ID, 0).expect("game exits");
        let ended =
            tokio::time::timeout(Duration::from_secs(3), rx.recv()).await.expect("signal in time").expect("open");
        assert!(!ended.active);

        det.stop();
        crate::platform::windows::registry::delete_tree(&base);
    }

    #[test]
    fn a_missing_steam_key_is_an_error_not_a_panic() {
        let (_tx, rx) = watch::channel(Arc::new(Config::default()));
        let (sig_tx, _sig_rx) = mpsc::unbounded_channel();
        let mut det = SteamDetector::with_key(r"Software\VRAMGatekeeperTests\does-not-exist");
        assert!(det.start(sig_tx, rx).is_err());
    }
}
