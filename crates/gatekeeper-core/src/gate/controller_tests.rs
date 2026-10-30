//! State-machine tests for the gate controller (spec §22.2).
//!
//! Each test drives the real controller with fake processes under a paused
//! clock, so timers (cooldown, drain, sustain) run instantly and
//! deterministically. Test names say which row of spec §7 or §21 they cover.

use std::time::Duration;

use serde_json::json;

use super::rejection::RejectKind;
use super::test_support::{Harness, config, harness};
use crate::config::schema::{DrainPolicy, GamePolicy, OverflowAction, PressureScope};
use crate::detect::GameSignal;
use crate::gpu::{ProcessVram, VramSample};

/// Seconds helper.
fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

/// Sends pressure-level samples once a second for `seconds` seconds.
async fn pressure_for(h: &Harness, used: u64, total: u64, seconds: u64) {
    for _ in 0..=seconds {
        h.handle.vram_sample(VramSample { total_mib: total, used_mib: used, ..VramSample::default() });
        tokio::time::sleep(secs(1)).await;
    }
}

/// Starts a game and waits for the gate to react.
async fn start_game(h: &Harness) {
    h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));
    h.settle().await;
}

/// Ends the game and waits out the cooldown.
async fn end_game(h: &Harness) {
    h.handle.game_signal(GameSignal::inactive("process_list"));
    tokio::time::sleep(secs(31)).await;
}

// ------------------------------------------------------------- admission

#[tokio::test(start_paused = true)]
async fn unknown_model_is_rejected_with_404_reason() {
    let h = harness(config(&["a"]));
    let err = h.handle.admit("nope").await.expect_err("unknown");
    assert_eq!(err.kind, RejectKind::UnknownModel);
}

#[tokio::test(start_paused = true)]
async fn request_loads_model_on_demand_and_counts_inflight() {
    let h = harness(config(&["a"]));
    assert_eq!(h.state_of("a").await, "unloaded");
    let adm = h.handle.admit("a").await.expect("admitted");
    assert_eq!(adm.model, "a");
    assert_eq!(h.state_of("a").await, "ready");
    assert_eq!(h.status().await.models[0].inflight, 1);
    drop(adm);
    h.settle().await;
    assert_eq!(h.status().await.models[0].inflight, 0);
    assert_eq!(h.launcher.launches("a"), 1);
}

#[tokio::test(start_paused = true)]
async fn alias_routes_case_insensitively() {
    let mut c = config(&["a"]);
    c.models[0].aliases = vec!["Chat".into()];
    let h = harness(c);
    assert_eq!(h.handle.admit("chat").await.expect("admitted").model, "a");
}

#[tokio::test(start_paused = true)]
async fn concurrent_requests_share_one_launch() {
    let h = harness(config(&["a"]));
    let (x, y, z) = tokio::join!(h.handle.admit("a"), h.handle.admit("a"), h.handle.admit("a"));
    assert!(x.is_ok() && y.is_ok() && z.is_ok());
    assert_eq!(h.launcher.launches("a"), 1);
    assert_eq!(h.status().await.models[0].inflight, 3);
}

#[tokio::test(start_paused = true)]
async fn slow_load_times_out_with_load_timeout() {
    let mut c = config(&["a"]);
    c.models[0].lifecycle.load_timeout_seconds = 5;
    let h = harness(c);
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let err = h.handle.admit("a").await.expect_err("timeout");
    assert_eq!(err.kind, RejectKind::LoadTimeout);
}

// ------------------------------------------------------------------ games

#[tokio::test(start_paused = true)]
async fn game_start_unloads_idle_model_and_rejects_requests_then_reloads_after_cooldown() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("admitted"));
    h.settle().await;

    start_game(&h).await; // FR-GAME-2, FR-GAME-4
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(!h.launcher.running("a"), "process killed so VRAM is freed");

    let err = h.handle.admit("a").await.expect_err("gated"); // FR-GAME-5
    assert_eq!(err.kind, RejectKind::GameRunning);
    assert!(err.message.contains("eldenring.exe"));
    assert_eq!(err.retry_after_seconds, Some(30));
    assert_eq!(err.detail["game"], "eldenring.exe");

    h.handle.game_signal(GameSignal::inactive("process_list"));
    tokio::time::sleep(secs(10)).await;
    assert!(h.handle.admit("a").await.is_err(), "still gated during cooldown");

    tokio::time::sleep(secs(25)).await;
    assert!(h.handle.admit("a").await.is_ok(), "reloads on demand after cooldown (FR-GAME-6)");
    assert_eq!(h.launcher.launches("a"), 2);
}

#[tokio::test(start_paused = true)]
async fn drain_finish_waits_for_inflight_without_limit() {
    let mut c = config(&["a"]);
    c.draining.policy = DrainPolicy::Finish;
    let h = harness(c);
    let adm = h.handle.admit("a").await.expect("admitted");

    start_game(&h).await;
    assert_eq!(h.state_of("a").await, "draining");
    tokio::time::sleep(secs(3600)).await;
    assert_eq!(h.state_of("a").await, "draining", "no time limit under `finish`");
    assert!(!adm.cancel.is_cancelled());

    drop(adm); // FR-GAME-3: finish, then unload
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn drain_wait_then_cancel_cancels_after_timeout() {
    let mut c = config(&["a"]);
    c.draining.policy = DrainPolicy::WaitThenCancel;
    c.draining.timeout_seconds = 30;
    let h = harness(c);
    let adm = h.handle.admit("a").await.expect("admitted");

    start_game(&h).await;
    tokio::time::sleep(secs(20)).await;
    assert!(!adm.cancel.is_cancelled());
    assert_eq!(h.state_of("a").await, "draining");

    tokio::time::sleep(secs(11)).await;
    assert!(adm.cancel.is_cancelled());
    let info = adm.cancel.info().expect("reason");
    assert_eq!(info.cause.as_str(), "game");
    assert!(info.message.contains("eldenring.exe"));
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(!h.launcher.running("a"));
}

#[tokio::test(start_paused = true)]
async fn drain_wait_then_cancel_finishes_early_if_requests_complete() {
    let h = harness(config(&["a"]));
    let adm = h.handle.admit("a").await.expect("admitted");
    start_game(&h).await;
    tokio::time::sleep(secs(5)).await;
    drop(adm);
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn drain_cancel_policy_cancels_immediately() {
    let mut c = config(&["a"]);
    c.draining.policy = DrainPolicy::Cancel;
    let h = harness(c);
    let adm = h.handle.admit("a").await.expect("admitted");
    start_game(&h).await;
    assert!(adm.cancel.is_cancelled());
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn game_during_starting_kills_child_and_rejects_waiters() {
    let h = harness(config(&["a"]));
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    assert_eq!(h.state_of("a").await, "starting");

    start_game(&h).await; // spec §21: kill immediately
    let err = waiting.await.expect("join").expect_err("rejected");
    assert_eq!(err.kind, RejectKind::GameRunning);
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(!h.launcher.running("a"));
}

#[tokio::test(start_paused = true)]
async fn keep_policy_model_ignores_games() {
    let mut c = config(&["a"]);
    c.models[0].lifecycle.game_policy = GamePolicy::Keep; // FR-GAME-7
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("admitted"));
    start_game(&h).await;
    assert_eq!(h.state_of("a").await, "ready");
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn gate_stays_closed_until_every_source_is_negative() {
    let h = harness(config(&["a"]));
    h.handle.game_signal(GameSignal::active("process_list", "a.exe"));
    h.handle.game_signal(GameSignal::active("steam", "A"));
    h.handle.game_signal(GameSignal::inactive("process_list"));
    tokio::time::sleep(secs(120)).await;
    assert!(h.handle.admit("a").await.is_err(), "steam still reports a game (spec §21)");
    h.handle.game_signal(GameSignal::inactive("steam"));
    tokio::time::sleep(secs(31)).await;
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn relaunching_exe_inside_cooldown_does_not_reopen_gate() {
    let h = harness(config(&["a"]));
    start_game(&h).await;
    h.handle.game_signal(GameSignal::inactive("process_list"));
    tokio::time::sleep(secs(10)).await;
    h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));
    tokio::time::sleep(secs(60)).await;
    assert_eq!(h.handle.admit("a").await.expect_err("still gated").kind, RejectKind::GameRunning);
}

#[tokio::test(start_paused = true)]
async fn allow_anyway_lasts_until_the_session_ends() {
    let h = harness(config(&["a"]));
    start_game(&h).await;
    h.handle.allow_anyway(); // FR-APP-3
    h.settle().await;
    assert!(h.handle.admit("a").await.is_ok());

    end_game(&h).await;
    start_game(&h).await;
    assert!(h.status().await.session_active);
    assert_eq!(h.state_of("a").await, "unloaded", "a new session is gated again");
}

#[tokio::test(start_paused = true)]
async fn disabled_game_gating_ignores_signals() {
    let mut c = config(&["a"]);
    c.games.enabled = false;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("admitted"));
    start_game(&h).await;
    assert_eq!(h.state_of("a").await, "ready");
}

#[tokio::test(start_paused = true)]
async fn preload_after_game_loads_models_when_session_ends() {
    let mut c = config(&["a"]);
    c.models[0].lifecycle.preload_after_game = true;
    let h = harness(c);
    start_game(&h).await;
    end_game(&h).await;
    h.settle().await;
    assert_eq!(h.state_of("a").await, "ready");
}

#[tokio::test(start_paused = true)]
async fn preload_on_start_loads_immediately() {
    let mut c = config(&["a"]);
    c.models[0].lifecycle.preload_on_start = true;
    let h = harness(c);
    h.settle().await;
    assert_eq!(h.state_of("a").await, "ready");
}

// ------------------------------------------------------------------- pause

#[tokio::test(start_paused = true)]
async fn pause_unloads_and_rejects_until_resumed() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("admitted"));
    h.handle.set_paused(true);
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
    assert_eq!(h.handle.admit("a").await.expect_err("paused").kind, RejectKind::Paused);
    h.handle.set_paused(false);
    h.settle().await;
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn unload_and_hold_rejects_until_released() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("admitted"));
    h.handle.unload("a", true).await;
    h.settle().await;
    assert_eq!(h.handle.admit("a").await.expect_err("held").kind, RejectKind::Paused);
    h.handle.release_hold("a");
    h.settle().await;
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn plain_unload_reloads_on_next_request() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("admitted"));
    h.handle.unload("a", false).await;
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(h.handle.admit("a").await.is_ok());
    assert_eq!(h.launcher.launches("a"), 2);
}

// -------------------------------------------------------------- VRAM pressure

#[tokio::test(start_paused = true)]
async fn pressure_unloads_lru_model_and_next_request_reloads() {
    let h = harness(config(&["a", "b"]));
    drop(h.handle.admit("a").await.expect("a"));
    tokio::time::sleep(secs(5)).await;
    drop(h.handle.admit("b").await.expect("b"));

    pressure_for(&h, 950, 1000, 4).await; // FR-VRAM-2
    assert_eq!(h.state_of("a").await, "unloaded", "least recently used goes first");
    assert_eq!(h.state_of("b").await, "ready");

    // Pressure gone: a request reloads a normally (FR-VRAM-3).
    h.handle.vram_sample(VramSample { total_mib: 1000, used_mib: 300, ..VramSample::default() });
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn short_pressure_spike_is_ignored() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    pressure_for(&h, 950, 1000, 1).await;
    h.handle.vram_sample(VramSample { total_mib: 1000, used_mib: 100, ..VramSample::default() });
    h.settle().await;
    assert_eq!(h.state_of("a").await, "ready");
}

#[tokio::test(start_paused = true)]
async fn pressure_scope_all_unloads_everything() {
    let mut c = config(&["a", "b"]);
    c.vram.pressure.scope = PressureScope::All;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("a"));
    drop(h.handle.admit("b").await.expect("b"));
    pressure_for(&h, 990, 1000, 4).await;
    assert_eq!(h.state_of("a").await, "unloaded");
    assert_eq!(h.state_of("b").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn pressure_evicts_pinned_models_last() {
    let mut c = config(&["a", "b"]);
    c.models[0].lifecycle.pinned = true;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("a"));
    tokio::time::sleep(secs(5)).await;
    drop(h.handle.admit("b").await.expect("b"));
    pressure_for(&h, 950, 1000, 4).await;
    assert_eq!(h.state_of("a").await, "ready", "pinned survives although it is older");
    assert_eq!(h.state_of("b").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn requests_during_pressure_drain_are_queued_then_served_by_reload() {
    let h = harness(config(&["a"]));
    let adm = h.handle.admit("a").await.expect("a");
    pressure_for(&h, 950, 1000, 4).await;
    assert_eq!(h.state_of("a").await, "draining");

    let queued = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    assert_eq!(h.status().await.models[0].queued, 1, "queued, not rejected (spec §7.2)");

    drop(adm);
    let reloaded = queued.await.expect("join").expect("served after reload");
    assert_eq!(reloaded.model, "a");
    assert_eq!(h.launcher.launches("a"), 2);
}

#[tokio::test(start_paused = true)]
async fn pressure_disabled_never_unloads() {
    let mut c = config(&["a"]);
    c.vram.pressure.enabled = false;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("a"));
    pressure_for(&h, 999, 1000, 10).await;
    assert_eq!(h.state_of("a").await, "ready");
}

// ---------------------------------------------------------------- overflow

#[tokio::test(start_paused = true)]
async fn layer_overflow_at_load_errors_the_triggering_request() {
    let h = harness(config(&["a"]));
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    h.launcher.log("a", "load_tensors: offloaded 20/33 layers to GPU"); // FR-VRAM-4
    let err = waiting.await.expect("join").expect_err("overflow error");
    assert_eq!(err.kind, RejectKind::RequestCancelled);
    assert_eq!(err.detail["cause"], "overflow");
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");

    // FR-VRAM-5: the next request loads normally.
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn explicit_gpu_layer_count_is_not_overflow() {
    let mut c = config(&["a"]);
    c.models[0].llama_args.insert("n-gpu-layers".into(), json!(20));
    let h = harness(c);
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    h.launcher.log("a", "load_tensors: offloaded 20/33 layers to GPU");
    h.settle().await;
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(waiting.await.expect("join").is_ok());
    assert_eq!(h.status().await.models[0].gpu_layers, Some((20, 33)));
    assert_eq!(h.status().await.models[0].overflow, None);
}

#[tokio::test(start_paused = true)]
async fn keep_running_overflow_only_flags_the_model() {
    let mut c = config(&["a"]);
    c.vram.overflow.action = OverflowAction::KeepRunning;
    let h = harness(c);
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    h.launcher.log("a", "load_tensors: offloaded 1/33 layers to GPU");
    h.settle().await;
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(waiting.await.expect("join").is_ok(), "model keeps running");
    assert!(h.status().await.models[0].overflow.is_some(), "but is flagged");
}

#[tokio::test(start_paused = true)]
async fn per_model_override_beats_global_overflow_action() {
    let mut c = config(&["a"]);
    c.models[0].vram_overrides.overflow_action = Some(OverflowAction::KeepRunning);
    let h = harness(c);
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    h.launcher.log("a", "load_tensors: offloaded 1/33 layers to GPU");
    h.settle().await;
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(waiting.await.expect("join").is_ok());
}

#[tokio::test(start_paused = true)]
async fn driver_spill_cancels_inflight_and_unloads() {
    let h = harness(config(&["a"]));
    let adm = h.handle.admit("a").await.expect("a");
    let pid = h.launcher.last("a").expect("proc").pid;
    tokio::time::sleep(secs(6)).await;

    let sample = |shared: u64| VramSample {
        total_mib: 32_000,
        used_mib: 10_000,
        per_process: vec![ProcessVram { pid, dedicated_mib: 8_000, shared_mib: shared }],
        ..VramSample::default()
    };
    h.handle.vram_sample(sample(100)); // baseline after settling (spec §9.5)
    h.settle().await;
    for _ in 0..3 {
        h.handle.vram_sample(sample(2_000)); // > baseline + 512 MiB, three samples
        h.settle().await;
    }
    assert!(adm.cancel.is_cancelled(), "overflow always cancels in-flight (spec §7.4)");
    assert_eq!(adm.cancel.info().expect("reason").cause.as_str(), "overflow");

    // The process is dead but the driver has not freed its VRAM yet, so the
    // model is not reported unloaded (spec §9.2) until usage drops.
    assert_eq!(h.state_of("a").await, "stopping");
    h.handle.vram_sample(VramSample { total_mib: 32_000, used_mib: 1_500, ..VramSample::default() });
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn stop_completes_after_release_timeout_if_usage_never_drops() {
    let h = harness(config(&["a"]));
    h.learn_footprint("a", 8_000);
    drop(h.handle.admit("a").await.expect("a"));
    h.handle.vram_sample(VramSample { total_mib: 32_000, used_mib: 10_000, ..VramSample::default() });
    h.settle().await;
    h.handle.unload("a", false).await;
    h.settle().await;
    assert_eq!(h.state_of("a").await, "stopping");
    tokio::time::sleep(secs(4)).await; // stop_release_wait_ms = 3000
    assert_eq!(h.state_of("a").await, "unloaded");
}

// ---------------------------------------------------------------- loop guard

#[tokio::test(start_paused = true)]
async fn loop_guard_holds_model_after_repeated_pressure_unloads_and_releases_when_vram_frees() {
    let h = harness(config(&["a"]));
    for _ in 0..3 {
        drop(h.handle.admit("a").await.expect("loads"));
        pressure_for(&h, 950, 1000, 4).await;
    }
    assert_eq!(h.state_of("a").await, "held"); // FR-VRAM-6
    let err = h.handle.admit("a").await.expect_err("held");
    assert_eq!(err.kind, RejectKind::VramGuard);
    assert_eq!(err.retry_after_seconds, Some(15));

    // Not enough free VRAM: stays held.
    for _ in 0..15 {
        h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 9_800, ..VramSample::default() });
        tokio::time::sleep(secs(1)).await;
    }
    assert_eq!(h.state_of("a").await, "held");

    // Plenty free for the sustain period: released and usable.
    for _ in 0..12 {
        h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 1_000, ..VramSample::default() });
        tokio::time::sleep(secs(1)).await;
    }
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn loop_guard_can_be_released_by_the_user() {
    let h = harness(config(&["a"]));
    for _ in 0..3 {
        drop(h.handle.admit("a").await.expect("loads"));
        pressure_for(&h, 950, 1000, 4).await;
    }
    assert_eq!(h.state_of("a").await, "held");
    h.handle.release_hold("a");
    h.settle().await;
    assert!(h.handle.admit("a").await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn loop_guard_disabled_never_holds() {
    let mut c = config(&["a"]);
    c.vram.loop_guard.enabled = false;
    let h = harness(c);
    for _ in 0..4 {
        drop(h.handle.admit("a").await.expect("loads"));
        pressure_for(&h, 950, 1000, 4).await;
    }
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn unloads_outside_the_window_do_not_trip_the_guard() {
    let h = harness(config(&["a"]));
    for _ in 0..4 {
        drop(h.handle.admit("a").await.expect("loads"));
        pressure_for(&h, 950, 1000, 4).await;
        tokio::time::sleep(secs(300)).await; // 5 min apart, window is 10
    }
    assert_ne!(h.state_of("a").await, "held");
}

// ----------------------------------------------------------------- make room

#[tokio::test(start_paused = true)]
async fn make_room_evicts_idle_lru_model_before_loading_another() {
    let h = harness(config(&["a", "b"]));
    h.learn_footprint("a", 6_000);
    h.learn_footprint("b", 5_000);
    drop(h.handle.admit("a").await.expect("a"));
    h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 6_500, ..VramSample::default() });
    h.settle().await;

    let b = h.handle.admit("b").await.expect("b loads after a is evicted"); // FR-VRAM-7
    assert_eq!(b.model, "b");
    assert_eq!(h.state_of("a").await, "unloaded");
    assert!(!h.launcher.running("a"));
}

#[tokio::test(start_paused = true)]
async fn make_room_never_evicts_busy_or_pinned_models() {
    let mut c = config(&["a", "b"]);
    c.models[0].lifecycle.pinned = true;
    let h = harness(c);
    h.learn_footprint("a", 6_000);
    h.learn_footprint("b", 5_000);
    let busy = h.handle.admit("a").await.expect("a");
    h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 6_500, ..VramSample::default() });
    h.settle().await;

    // Step 4 of spec §13.4: starts anyway rather than evicting.
    assert!(h.handle.admit("b").await.is_ok());
    assert_eq!(h.state_of("a").await, "ready");
    drop(busy);
}

#[tokio::test(start_paused = true)]
async fn make_room_disabled_just_starts() {
    let mut c = config(&["a", "b"]);
    c.vram.make_room.enabled = false;
    let h = harness(c);
    h.learn_footprint("a", 6_000);
    h.learn_footprint("b", 5_000);
    drop(h.handle.admit("a").await.expect("a"));
    h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 6_500, ..VramSample::default() });
    h.settle().await;
    assert!(h.handle.admit("b").await.is_ok());
    assert_eq!(h.state_of("a").await, "ready");
}

// -------------------------------------------------------------- idle/failure

#[tokio::test(start_paused = true)]
async fn idle_model_unloads_after_ttl() {
    let mut c = config(&["a"]);
    c.models[0].lifecycle.idle_unload_minutes = 1;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("a"));
    tokio::time::sleep(secs(45)).await;
    assert_eq!(h.state_of("a").await, "ready");
    tokio::time::sleep(secs(30)).await;
    assert_eq!(h.state_of("a").await, "unloaded");
}

#[tokio::test(start_paused = true)]
async fn idle_ttl_zero_never_unloads_and_busy_models_are_kept() {
    let mut c = config(&["a", "b"]);
    c.models[0].lifecycle.idle_unload_minutes = 0;
    c.models[1].lifecycle.idle_unload_minutes = 1;
    let h = harness(c);
    drop(h.handle.admit("a").await.expect("a"));
    let busy = h.handle.admit("b").await.expect("b");
    tokio::time::sleep(secs(600)).await;
    assert_eq!(h.state_of("a").await, "ready");
    assert_eq!(h.state_of("b").await, "ready", "busy models are never idle-unloaded");
    drop(busy);
}

#[tokio::test(start_paused = true)]
async fn crash_cancels_inflight_and_next_request_restarts() {
    let h = harness(config(&["a"]));
    let adm = h.handle.admit("a").await.expect("a");
    h.launcher.crash("a");
    h.settle().await;
    assert!(adm.cancel.is_cancelled());
    assert_eq!(adm.cancel.info().expect("reason").cause.as_str(), "crash");
    assert_eq!(h.state_of("a").await, "unloaded");
    drop(adm);
    assert!(h.handle.admit("a").await.is_ok());
    assert_eq!(h.launcher.launches("a"), 2);
}

#[tokio::test(start_paused = true)]
async fn start_failure_backs_off_exponentially_then_recovers() {
    let h = harness(config(&["a"]));
    h.launcher.fail_next("bad args");
    let err = h.handle.admit("a").await.expect_err("fails");
    assert_eq!(err.kind, RejectKind::ModelLoadFailed);
    assert_eq!(err.retry_after_seconds, Some(10));
    assert_eq!(h.state_of("a").await, "failed");

    let again = h.handle.admit("a").await.expect_err("in backoff");
    assert_eq!(again.kind, RejectKind::ModelLoadFailed);
    assert!(h.launcher.launches("a") == 0, "no retry during backoff");

    tokio::time::sleep(secs(11)).await;
    h.launcher.fail_next("still bad");
    let second = h.handle.admit("a").await.expect_err("fails again");
    assert_eq!(second.retry_after_seconds, Some(20), "backoff doubles");

    tokio::time::sleep(secs(21)).await;
    assert!(h.handle.admit("a").await.is_ok(), "recovers once the problem is fixed");
}

#[tokio::test(start_paused = true)]
async fn exit_during_startup_is_a_failed_start_with_log_tail() {
    let h = harness(config(&["a"]));
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    let waiting = tokio::spawn({
        let handle = h.handle.clone();
        async move { handle.admit("a").await }
    });
    h.settle().await;
    h.launcher.log("a", "error: unknown argument --bogus");
    h.settle().await;
    h.launcher.crash("a");
    let err = waiting.await.expect("join").expect_err("failed start");
    assert_eq!(err.kind, RejectKind::ModelLoadFailed);
    assert!(err.message.contains("exited during startup"));
}

// -------------------------------------------------------------------- config

#[tokio::test(start_paused = true)]
async fn changed_launch_settings_mark_restart_needed_and_restart_applies_them() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    let mut next = h.config.clone();
    next.models[0].llama_args.insert("ctx-size".into(), json!(4096));
    h.handle.set_config(std::sync::Arc::new(next));
    h.settle().await;
    assert!(h.status().await.models[0].needs_restart);

    h.handle.restart("a").await;
    h.settle().await;
    assert_eq!(h.launcher.launches("a"), 2);
    assert!(!h.status().await.models[0].needs_restart);
}

#[tokio::test(start_paused = true)]
async fn switching_the_engine_marks_a_loaded_model_for_restart() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    assert!(!h.status().await.models[0].needs_restart);

    let mut next = h.config.clone();
    next.engine.llama_server_dir = Some("C:/engines/b11827-cuda".into());
    h.handle.set_config(std::sync::Arc::new(next));
    h.settle().await;
    assert!(h.status().await.models[0].needs_restart, "a loaded model still runs the old engine binary");

    // Going back to the engine it was started with clears the flag.
    h.handle.set_config(std::sync::Arc::new(h.config.clone()));
    h.settle().await;
    assert!(!h.status().await.models[0].needs_restart);

    let mut device = h.config.clone();
    device.engine.device = Some("Vulkan0".into());
    h.handle.set_config(std::sync::Arc::new(device));
    h.settle().await;
    assert!(h.status().await.models[0].needs_restart, "a different --device also needs a restart");
}

#[tokio::test(start_paused = true)]
async fn disabling_a_model_unloads_it_and_rejects_requests() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    let mut next = h.config.clone();
    next.models[0].enabled = false;
    h.handle.set_config(std::sync::Arc::new(next));
    h.settle().await;
    assert_eq!(h.state_of("a").await, "unloaded");
    assert_eq!(h.handle.admit("a").await.expect_err("disabled").kind, RejectKind::UnknownModel);
}

#[tokio::test(start_paused = true)]
async fn removed_models_disappear_from_status_once_unloaded() {
    let h = harness(config(&["a", "b"]));
    drop(h.handle.admit("a").await.expect("a"));
    let mut next = h.config.clone();
    next.models.retain(|m| m.id != "a");
    h.handle.set_config(std::sync::Arc::new(next));
    h.settle().await;
    let ids: Vec<String> = h.status().await.models.into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["b"], "the removed model's process is gone and its record dropped");
    assert!(!h.launcher.running("a"));
}

#[tokio::test(start_paused = true)]
async fn shutdown_kills_children_and_rejects_waiters() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    h.handle.shutdown().await;
    h.settle().await;
    assert!(!h.launcher.running("a"));
    assert_eq!(h.handle.admit("a").await.expect_err("down").kind, RejectKind::ShuttingDown);
}

// ------------------------------------------------------------ sleep and resume

#[tokio::test(start_paused = true)]
async fn after_a_resume_a_model_that_still_answers_is_left_alone() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    assert_eq!(h.launcher.launches("a"), 1);
    h.handle.system_resumed();
    tokio::time::sleep(secs(30)).await;
    assert_eq!(h.state_of("a").await, "ready");
    assert_eq!(h.launcher.launches("a"), 1, "no restart when the health check passes");
}

#[tokio::test(start_paused = true)]
async fn after_a_resume_a_model_that_stopped_answering_is_restarted() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    // The driver reset during sleep: the server no longer answers.
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.system_resumed();
    tokio::time::sleep(secs(2)).await;
    assert_eq!(h.launcher.launches("a"), 1, "it gets a few tries before being restarted");
    // Once it is healthy again after the restart, the replacement becomes ready.
    tokio::time::sleep(secs(8)).await;
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(secs(60)).await;
    assert_eq!(h.launcher.launches("a"), 2, "restarted exactly once");
    assert_eq!(h.state_of("a").await, "ready");
}

#[tokio::test(start_paused = true)]
async fn a_slow_wake_that_recovers_within_the_retries_does_not_restart() {
    let h = harness(config(&["a"]));
    drop(h.handle.admit("a").await.expect("a"));
    h.probe.ready.store(false, std::sync::atomic::Ordering::SeqCst);
    h.handle.system_resumed();
    tokio::time::sleep(secs(4)).await; // after the first retry, before the last
    h.probe.ready.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::time::sleep(secs(60)).await;
    assert_eq!(h.launcher.launches("a"), 1);
    assert_eq!(h.state_of("a").await, "ready");
}
