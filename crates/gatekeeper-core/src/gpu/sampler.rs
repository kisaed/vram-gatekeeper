//! The VRAM sampling loop (spec §12.3).
//!
//! Responsibility: read the GPU at the right cadence and feed the gate
//! controller. Fast (1 s) while a model is loaded, a game session is open or
//! a model is held; slow (10 s) otherwise, or off if configured.
//! Not responsible for: what the readings mean (see `gate::vram_rules`).
//!
//! Implements FR-VRAM-1, NFR-1.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::GpuMonitor;
use crate::config::schema::Config;
use crate::events::{AppEvent, EventBus};
use crate::gate::controller::{ControllerHandle, StatusSnapshot};
use crate::gate::model_state::ModelStateKind;

/// Delay before retrying after the counters fail, so a machine without
/// working counters does not spin (spec §21: VRAM features then stay off).
const ERROR_BACKOFF: Duration = Duration::from_secs(30);

/// True when fast sampling is warranted.
fn needs_fast_sampling(status: &StatusSnapshot) -> bool {
    status.session_active
        || status.models.iter().any(|m| !matches!(m.state, ModelStateKind::Unloaded | ModelStateKind::Failed))
}

/// Runs until `stop` fires.
pub async fn run(
    monitor: Arc<dyn GpuMonitor>,
    controller: ControllerHandle,
    config: watch::Receiver<Arc<Config>>,
    events: EventBus,
    stop: CancellationToken,
) {
    let mut warned = false;
    // Model and gate changes wake the loop so a model that just loaded is
    // sampled at the fast rate immediately, not after the idle sleep ends.
    let mut changes = events.subscribe();
    loop {
        let (active_ms, idle_ms, idle_enabled) = {
            let a = &config.borrow().advanced;
            (a.gpu_sample_interval_ms_active, a.gpu_sample_interval_ms_idle, a.sample_when_idle)
        };
        let Some(status) = controller.status().await else {
            return;
        };
        let fast = needs_fast_sampling(&status);

        let mut wait = Duration::from_millis(u64::from(if fast { active_ms } else { idle_ms }));
        if fast || idle_enabled {
            match monitor.sample().await {
                Ok(sample) => {
                    warned = false;
                    controller.vram_sample(sample);
                }
                Err(e) => {
                    if !warned {
                        warn!(error = %e, "GPU monitoring unavailable; VRAM rules are paused");
                        warned = true;
                    }
                    wait = ERROR_BACKOFF;
                }
            }
        }
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
            () = relevant_change(&mut changes) => {}
        }
    }
}

/// Completes when a model or the gate changes state.
async fn relevant_change(rx: &mut broadcast::Receiver<AppEvent>) {
    loop {
        match rx.recv().await {
            Ok(AppEvent::ModelState { .. } | AppEvent::Gate { .. }) => return,
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
            // The bus is gone: never wake from it again.
            Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::gate::test_support::{config, harness};
    use crate::gpu::{GpuError, VramSample};

    /// Counts samples.
    struct Counting(AtomicUsize, Mutex<bool>);

    #[async_trait]
    impl GpuMonitor for Counting {
        async fn sample(&self) -> Result<VramSample, GpuError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            if *self.1.lock().expect("lock") { Err(GpuError("no counters".into())) } else { Ok(VramSample::default()) }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn samples_slowly_when_idle_and_quickly_when_a_model_is_loaded() {
        let h = harness(config(&["a"]));
        let mon = Arc::new(Counting(AtomicUsize::new(0), Mutex::new(false)));
        let (_tx, rx) = watch::channel(Arc::new(h.config.clone()));
        let stop = CancellationToken::new();
        tokio::spawn(run(mon.clone(), h.handle.clone(), rx, h.events.clone(), stop.clone()));

        tokio::time::sleep(Duration::from_secs(35)).await;
        let idle_count = mon.0.load(Ordering::SeqCst);
        assert!((3..=5).contains(&idle_count), "10 s cadence while idle, got {idle_count}");

        let _adm = h.handle.admit("a").await.expect("loads");
        let before = mon.0.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(10)).await;
        let fast_count = mon.0.load(Ordering::SeqCst) - before;
        assert!(fast_count >= 8, "1 s cadence while loaded, got {fast_count}");
        stop.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn idle_sampling_can_be_switched_off() {
        let mut c = config(&["a"]);
        c.advanced.sample_when_idle = false;
        let h = harness(c.clone());
        let mon = Arc::new(Counting(AtomicUsize::new(0), Mutex::new(false)));
        let (_tx, rx) = watch::channel(Arc::new(c));
        let stop = CancellationToken::new();
        tokio::spawn(run(mon.clone(), h.handle.clone(), rx, h.events.clone(), stop.clone()));
        tokio::time::sleep(Duration::from_secs(60)).await;
        assert_eq!(mon.0.load(Ordering::SeqCst), 0);
        stop.cancel();
    }

    #[tokio::test(start_paused = true)]
    async fn counter_failures_back_off_instead_of_spinning() {
        let c = config(&["a"]);
        let h = harness(c.clone());
        let mon = Arc::new(Counting(AtomicUsize::new(0), Mutex::new(true)));
        let (_tx, rx) = watch::channel(Arc::new(c));
        let stop = CancellationToken::new();
        tokio::spawn(run(mon.clone(), h.handle.clone(), rx, h.events.clone(), stop.clone()));
        tokio::time::sleep(Duration::from_secs(95)).await;
        assert!(mon.0.load(Ordering::SeqCst) <= 4, "retries every 30 s");
        stop.cancel();
    }
}
