//! Foreground-change and exclusive full-screen detector (spec §11.2, §11.4).
//!
//! Responsibility: wake the process-list scanner the instant the foreground
//! window changes (NFR-2), and report a game when an exclusive full-screen
//! Direct3D application is running.
//! Not responsible for: the process list itself (see `process_scan`) or the
//! heavy-GPU heuristic.
//!
//! Plain `QUNS_BUSY` (video, presentations) is *not* a game on its own
//! (spec F6); only exclusive Direct3D full-screen is.
//!
//! Implements FR-GAME-1, NFR-2.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{GameDetector, GameSignal};
use crate::config::schema::Config;
use crate::error::CoreResult;
use crate::platform::windows::foreground::{FullscreenState, foreground_pid, fullscreen_state, start_foreground_hook};
use crate::platform::windows::process_list::list_processes;

/// Source name in signals.
pub const SOURCE: &str = "fullscreen";

/// Decides whether the full-screen state means "a game" (spec §11.4).
///
/// Returns the display name if so. `foreground_exe` is the foreground
/// process's file name when known; names in `ignored` never count.
pub fn fullscreen_game(
    state: FullscreenState,
    foreground_exe: Option<&str>,
    ignored: &[String],
    enabled: bool,
) -> Option<String> {
    if !enabled || state != FullscreenState::ExclusiveD3d {
        return None;
    }
    if foreground_exe.is_some_and(|exe| ignored.iter().any(|i| i.eq_ignore_ascii_case(exe))) {
        return None;
    }
    Some(foreground_exe.map_or_else(|| "a full-screen game".to_owned(), str::to_owned))
}

/// File name of the process with `pid`, from a Toolhelp snapshot.
fn exe_name_of(pid: u32) -> Option<String> {
    list_processes().ok()?.into_iter().find(|p| p.pid == pid).map(|p| p.name)
}

/// The detector.
pub struct ForegroundDetector {
    /// Wakes the process-list scanner on foreground changes.
    scan_trigger: Option<Arc<Notify>>,
    /// Stops the background task.
    stop: CancellationToken,
}

impl ForegroundDetector {
    /// Creates a detector. Pass the process scanner's trigger so a foreground
    /// change causes an immediate scan.
    pub fn new(scan_trigger: Option<Arc<Notify>>) -> Self {
        Self { scan_trigger, stop: CancellationToken::new() }
    }
}

impl GameDetector for ForegroundDetector {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn start(
        &mut self,
        signals: mpsc::UnboundedSender<GameSignal>,
        config: watch::Receiver<Arc<Config>>,
    ) -> CoreResult<()> {
        let (trigger, stop) = (self.scan_trigger.clone(), self.stop.clone());
        tokio::spawn(run(trigger, stop, signals, config));
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.cancel();
    }
}

/// Background loop: hook events plus a slow poll for full-screen changes that
/// happen without a foreground change.
async fn run(
    trigger: Option<Arc<Notify>>,
    stop: CancellationToken,
    signals: mpsc::UnboundedSender<GameSignal>,
    config: watch::Receiver<Arc<Config>>,
) {
    let (event_tx, mut events) = mpsc::unbounded_channel::<u32>();
    // The hook is kept alive for the lifetime of this loop. If it cannot be
    // installed we still poll, just with the slower scan-interval latency.
    let hook = if config.borrow().games.detect_foreground_changes {
        match start_foreground_hook(event_tx) {
            Ok(h) => Some(h),
            Err(e) => {
                warn!(error = %e, "foreground hook unavailable; falling back to polling");
                None
            }
        }
    } else {
        None
    };

    let mut reported: Option<String> = None;
    loop {
        let cfg = config.borrow().clone();
        let games = &cfg.games;
        let enabled = games.enabled && games.detect_exclusive_fullscreen;

        let found = if enabled {
            // Ask the cheap question first: most of the time nothing is
            // full-screen, and naming the foreground app needs a whole process
            // snapshot, which would be wasted work at every poll.
            let state = fullscreen_state().unwrap_or(FullscreenState::None);
            let fg_exe =
                if state == FullscreenState::ExclusiveD3d { foreground_pid().and_then(exe_name_of) } else { None };
            fullscreen_game(state, fg_exe.as_deref(), &games.ignored_executables, true)
        } else {
            None
        };
        if found != reported {
            let signal = match &found {
                Some(name) => GameSignal::active(SOURCE, name),
                None => GameSignal::inactive(SOURCE),
            };
            if signals.send(signal).is_err() {
                return;
            }
            reported = found;
        }

        let wait = Duration::from_secs(u64::from(games.scan_interval_seconds.max(1)));
        tokio::select! {
            () = stop.cancelled() => break,
            Some(_pid) = events.recv() => {
                // Foreground changed: scan processes right now (NFR-2).
                if let Some(t) = &trigger {
                    t.notify_one();
                }
            }
            () = tokio::time::sleep(wait) => {}
        }
    }
    drop(hook);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_exclusive_d3d_counts_as_a_game() {
        assert_eq!(
            fullscreen_game(FullscreenState::ExclusiveD3d, Some("game.exe"), &[], true).as_deref(),
            Some("game.exe")
        );
        assert_eq!(
            fullscreen_game(FullscreenState::Busy, Some("vlc.exe"), &[], true),
            None,
            "a full-screen video is not a game (F6)"
        );
        assert_eq!(fullscreen_game(FullscreenState::None, None, &[], true), None);
    }

    #[test]
    fn disabled_or_ignored_apps_do_not_count() {
        assert_eq!(fullscreen_game(FullscreenState::ExclusiveD3d, Some("game.exe"), &[], false), None);
        assert_eq!(
            fullscreen_game(FullscreenState::ExclusiveD3d, Some("Player.exe"), &["player.EXE".into()], true),
            None
        );
    }

    #[test]
    fn unknown_foreground_app_still_gets_a_label() {
        assert_eq!(
            fullscreen_game(FullscreenState::ExclusiveD3d, None, &[], true).as_deref(),
            Some("a full-screen game")
        );
    }
}
