//! Heavy-GPU heuristic detector (spec §11.5, opt-in).
//!
//! Responsibility: catch games that are not on the list and run borderless by
//! watching the *foreground* program's 3D engine load and dedicated memory,
//! sustained for a few seconds.
//! Not responsible for the other detectors; this one is off by default because
//! 3D editors, video tools and WebGL pages can trigger it.
//!
//! Implements FR-GAME-1.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::{GameDetector, GameSignal};
use crate::config::schema::{Config, HeavyGpuHeuristic};
use crate::error::CoreResult;
use crate::gpu::instance::process_3d_utilization;
use crate::gpu::windows_monitor::WindowsGpuMonitor;
use crate::platform::windows::foreground::foreground_pid;
use crate::platform::windows::pdh::EngineQuery;
use crate::platform::windows::process_list::list_processes;
use crate::sync::MutexExt;

/// Source name in signals.
pub const SOURCE: &str = "gpu_heuristic";

/// Programs that are never treated as games by this heuristic (spec §11.5):
/// the desktop, browsers, our own processes and common streaming tools. The
/// user can add more in `games.ignored_executables`.
pub const BUILTIN_EXCLUDED: [&str; 18] = [
    "dwm.exe",
    "explorer.exe",
    "vram-gatekeeper.exe",
    "llama-server.exe",
    "msedgewebview2.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "opera.exe",
    "vivaldi.exe",
    "discord.exe",
    "obs64.exe",
    "obs32.exe",
    "steamwebhelper.exe",
    "searchhost.exe",
    "shellexperiencehost.exe",
    "applicationframehost.exe",
];

/// Seconds the thresholds may lapse before the signal is withdrawn, so a
/// loading screen does not end the session.
const RELEASE_SECONDS: u32 = 5;

/// Does this foreground program look like a game right now?
pub fn heuristic_hit(
    util_percent: f64,
    dedicated_mib: u64,
    exe: &str,
    cfg: &HeavyGpuHeuristic,
    ignored: &[String],
) -> bool {
    if BUILTIN_EXCLUDED.iter().any(|b| b.eq_ignore_ascii_case(exe))
        || ignored.iter().any(|i| i.eq_ignore_ascii_case(exe))
    {
        return false;
    }
    util_percent >= f64::from(cfg.min_3d_utilization_percent) && dedicated_mib >= u64::from(cfg.min_dedicated_mib)
}

/// Turns once-per-second hit/miss observations into a steady on/off signal.
#[derive(Debug, Default)]
pub struct Sustain {
    /// Consecutive seconds of hits.
    hits: u32,
    /// Consecutive seconds of misses while active.
    misses: u32,
    /// Currently reporting a game.
    active: bool,
}

impl Sustain {
    /// Feeds one observation. Returns `Some(true)` when the signal turns on
    /// (after `need` seconds of hits), `Some(false)` when it turns off (after
    /// `release` seconds of misses), else `None`.
    pub fn observe(&mut self, hit: bool, need: u32, release: u32) -> Option<bool> {
        if hit {
            self.misses = 0;
            self.hits += 1;
            if !self.active && self.hits >= need {
                self.active = true;
                return Some(true);
            }
        } else {
            self.hits = 0;
            if self.active {
                self.misses += 1;
                if self.misses >= release {
                    self.active = false;
                    self.misses = 0;
                    return Some(false);
                }
            }
        }
        None
    }
}

/// The detector.
pub struct HeavyGpuDetector {
    /// Reads per-process dedicated memory and knows the adapter.
    monitor: Arc<WindowsGpuMonitor>,
    /// Stops the background task.
    stop: CancellationToken,
}

impl HeavyGpuDetector {
    /// Creates the detector for the monitored adapter.
    pub fn new(monitor: Arc<WindowsGpuMonitor>) -> Self {
        Self { monitor, stop: CancellationToken::new() }
    }
}

impl GameDetector for HeavyGpuDetector {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn start(
        &mut self,
        signals: mpsc::UnboundedSender<GameSignal>,
        config: watch::Receiver<Arc<Config>>,
    ) -> CoreResult<()> {
        tokio::spawn(run(self.monitor.clone(), self.stop.clone(), signals, config));
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.cancel();
    }
}

/// File name of the process with `pid`.
fn exe_name_of(pid: u32) -> Option<String> {
    list_processes().ok()?.into_iter().find(|p| p.pid == pid).map(|p| p.name)
}

async fn run(
    monitor: Arc<WindowsGpuMonitor>,
    stop: CancellationToken,
    signals: mpsc::UnboundedSender<GameSignal>,
    config: watch::Receiver<Arc<Config>>,
) {
    // The engine counter is only opened once the heuristic is first enabled,
    // so a user who never turns it on pays nothing (spec §12.2).
    let query: Arc<Mutex<Option<EngineQuery>>> = Arc::new(Mutex::new(None));
    // Last foreground (pid, exe name), so the process list is read only when the pid changes.
    let name_cache: Arc<Mutex<(u32, String)>> = Arc::new(Mutex::new((0, String::new())));
    let mut sustain = Sustain::default();
    let mut reported: Option<String> = None;
    let mut last_name = String::new();

    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        let cfg = config.borrow().clone();
        let heuristic = &cfg.games.heavy_gpu_heuristic;
        if !(cfg.games.enabled && heuristic.enabled) {
            if reported.take().is_some() {
                sustain = Sustain::default();
                let _ = signals.send(GameSignal::inactive(SOURCE));
            }
            continue;
        }

        // One second of observation, off the async workers (PDH + Toolhelp).
        let (q, name_cache, mon, luid, cfg_now) =
            (query.clone(), name_cache.clone(), monitor.clone(), monitor.luid(), cfg.clone());
        let observation = tokio::task::spawn_blocking(move || {
            let mut guard = q.lock_ok();
            if guard.is_none() {
                *guard = EngineQuery::new().map_err(|e| warn!(error = %e, "heavy-GPU heuristic unavailable")).ok();
            }
            // Cheapest checks first: most seconds the foreground is a browser or
            // the desktop, and then neither the counters nor memory are read.
            let pid = foreground_pid()?;
            let exe = {
                let mut known = name_cache.lock_ok();
                if known.0 != pid {
                    *known = (pid, exe_name_of(pid)?);
                }
                known.1.clone()
            };
            if BUILTIN_EXCLUDED.iter().any(|b| b.eq_ignore_ascii_case(&exe))
                || cfg_now.games.ignored_executables.iter().any(|i| i.eq_ignore_ascii_case(&exe))
            {
                return Some((false, exe));
            }
            let engines = guard.as_mut()?.read().ok()?;
            let util = process_3d_utilization(&engines, pid, luid);
            drop(guard);
            let dedicated =
                mon.sample_blocking().ok().and_then(|s| s.process(pid).map(|p| p.dedicated_mib)).unwrap_or(0);
            Some((
                heuristic_hit(
                    util,
                    dedicated,
                    &exe,
                    &cfg_now.games.heavy_gpu_heuristic,
                    &cfg_now.games.ignored_executables,
                ),
                exe,
            ))
        })
        .await
        .ok()
        .flatten();

        let (hit, exe) = observation.unwrap_or((false, String::new()));
        if hit {
            last_name = exe;
        }
        match sustain.observe(hit, heuristic.sustain_seconds.max(1), RELEASE_SECONDS) {
            Some(true) => {
                reported = Some(last_name.clone());
                if signals.send(GameSignal::active(SOURCE, &last_name)).is_err() {
                    return;
                }
            }
            Some(false) => {
                reported = None;
                if signals.send(GameSignal::inactive(SOURCE)).is_err() {
                    return;
                }
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HeavyGpuHeuristic {
        HeavyGpuHeuristic {
            enabled: true,
            min_3d_utilization_percent: 50,
            min_dedicated_mib: 2048,
            sustain_seconds: 10,
        }
    }

    #[test]
    fn both_thresholds_must_be_met() {
        assert!(heuristic_hit(80.0, 4000, "game.exe", &cfg(), &[]));
        assert!(!heuristic_hit(30.0, 4000, "game.exe", &cfg(), &[]), "low 3D load");
        assert!(!heuristic_hit(80.0, 500, "game.exe", &cfg(), &[]), "little VRAM");
    }

    #[test]
    fn builtin_and_user_exclusions_never_count() {
        assert!(!heuristic_hit(99.0, 9000, "chrome.exe", &cfg(), &[]));
        assert!(!heuristic_hit(99.0, 9000, "DWM.EXE", &cfg(), &[]));
        assert!(!heuristic_hit(99.0, 9000, "blender.exe", &cfg(), &["Blender.exe".into()]));
        assert!(heuristic_hit(99.0, 9000, "blender.exe", &cfg(), &[]));
    }

    #[test]
    fn sustain_needs_continuous_hits_and_a_grace_period_to_release() {
        let mut s = Sustain::default();
        let mut fired = None;
        for second in 1..=10 {
            if let Some(v) = s.observe(true, 10, 5) {
                fired = Some((second, v));
            }
        }
        assert_eq!(fired, Some((10, true)), "turns on after 10 s of hits");
        // A short dip (loading screen) does not end it...
        for _ in 0..3 {
            assert_eq!(s.observe(false, 10, 5), None);
        }
        assert_eq!(s.observe(true, 10, 5), None);
        // ...but a sustained absence does.
        let mut off = None;
        for second in 1..=5 {
            if let Some(v) = s.observe(false, 10, 5) {
                off = Some((second, v));
            }
        }
        assert_eq!(off, Some((5, false)));
    }

    #[test]
    fn interrupted_hits_restart_the_count() {
        let mut s = Sustain::default();
        for _ in 0..9 {
            s.observe(true, 10, 5);
        }
        s.observe(false, 10, 5);
        assert_eq!(s.observe(true, 10, 5), None, "needs a fresh 10 s");
    }
}
