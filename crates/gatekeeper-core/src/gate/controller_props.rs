//! Property tests for the gate controller (spec §22.2).
//!
//! Random sequences of requests, game start/stop, VRAM pressure, crashes,
//! pausing and time jumps are run against the real controller (with fake
//! processes and a paused clock), and these invariants are checked after
//! every step:
//!
//! 1. **Counting:** each model's in-flight count equals the number of live
//!    (not cancelled) request guards the test holds for it.
//! 2. **No kill under a patient drain:** a live request implies its model's
//!    process is running. The controller never kills a model with requests it
//!    has not cancelled.
//! 3. **No admission while blocked:** a request that was pending when a game
//!    started or the gatekeeper was paused never resolves successfully in that
//!    step (for models that unload during games).
//!
//! and at the end, once the world is quiet:
//!
//! 4. **Every admit resolves exactly once** (none are left hanging).
//! 5. **Nothing is stuck:** no model is starting, draining or stopping, and
//!    in-flight and queued counts are zero.

use std::time::Duration;

use proptest::prelude::*;
use tokio::task::JoinHandle;

use super::controller::Admitted;
use super::model_state::ModelStateKind;
use super::rejection::Rejection;
use super::test_support::{Harness, config, harness};
use crate::config::schema::{DrainPolicy, GamePolicy};
use crate::detect::GameSignal;
use crate::gpu::VramSample;

/// Model ids used by the generated scenarios; `c` ignores games.
const MODELS: [&str; 3] = ["a", "b", "c"];

/// One step of a scenario.
#[derive(Debug, Clone)]
enum Op {
    /// Request model `MODELS[i]`.
    Admit(usize),
    /// Finish the `n`-th live request (modulo the number held).
    Drop(usize),
    /// A game starts.
    GameOn,
    /// The game exits.
    GameOff,
    /// Report high (`true`) or low (`false`) VRAM use for several seconds.
    Pressure(bool),
    /// The process of `MODELS[i]` crashes.
    Crash(usize),
    /// Let `n` seconds pass.
    Advance(u64),
    /// Pause or resume the gatekeeper.
    Pause(bool),
    /// "Allow anyway".
    Override,
    /// Unload everything.
    UnloadAll,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..MODELS.len()).prop_map(Op::Admit),
        3 => (0usize..8).prop_map(Op::Drop),
        2 => Just(Op::GameOn),
        2 => Just(Op::GameOff),
        2 => any::<bool>().prop_map(Op::Pressure),
        1 => (0..MODELS.len()).prop_map(Op::Crash),
        3 => (1u64..120).prop_map(Op::Advance),
        1 => any::<bool>().prop_map(Op::Pause),
        1 => Just(Op::Override),
        1 => Just(Op::UnloadAll),
    ]
}

/// A request waiting for its verdict.
struct Pending {
    model: usize,
    handle: JoinHandle<Result<Admitted, Rejection>>,
}

/// Collects verdicts that have arrived. Returns how many resolved `Ok`
/// together with the models they were for.
async fn collect(pending: &mut Vec<Pending>, live: &mut Vec<(usize, Admitted)>) -> Vec<usize> {
    let mut resolved_ok = Vec::new();
    let mut still = Vec::new();
    for p in pending.drain(..) {
        if p.handle.is_finished() {
            if let Ok(adm) = p.handle.await.expect("admit task must not panic") {
                resolved_ok.push(p.model);
                live.push((p.model, adm));
            }
        } else {
            still.push(p);
        }
    }
    *pending = still;
    resolved_ok
}

/// Runs one scenario and panics (failing the property) on a violation.
async fn run_scenario(ops: Vec<Op>, policy: DrainPolicy) {
    let mut cfg = config(&MODELS);
    cfg.draining.policy = policy;
    cfg.draining.timeout_seconds = 3;
    cfg.games.cooldown_seconds = 5;
    cfg.models[2].lifecycle.game_policy = GamePolicy::Keep;
    cfg.models[0].lifecycle.idle_unload_minutes = 1;
    let h = harness(cfg);

    let mut pending: Vec<Pending> = Vec::new();
    let mut live: Vec<(usize, Admitted)> = Vec::new();

    for (step, op) in ops.iter().enumerate() {
        let before_ok = collect(&mut pending, &mut live).await;
        let _ = before_ok;
        let blocking_op = matches!(op, Op::GameOn | Op::Pause(true));

        match op {
            Op::Admit(i) => {
                let handle = h.handle.clone();
                let name = MODELS[*i];
                pending.push(Pending { model: *i, handle: tokio::spawn(async move { handle.admit(name).await }) });
            }
            Op::Drop(n) => {
                if !live.is_empty() {
                    let idx = n % live.len();
                    drop(live.remove(idx));
                }
            }
            Op::GameOn => h.handle.game_signal(GameSignal::active("process_list", "game.exe")),
            Op::GameOff => h.handle.game_signal(GameSignal::inactive("process_list")),
            Op::Pressure(high) => {
                let used = if *high { 960 } else { 100 };
                for _ in 0..5 {
                    h.handle.vram_sample(VramSample { total_mib: 1000, used_mib: used, ..VramSample::default() });
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            Op::Crash(i) => h.launcher.crash(MODELS[*i]),
            Op::Advance(s) => tokio::time::sleep(Duration::from_secs(*s)).await,
            Op::Pause(p) => h.handle.set_paused(*p),
            Op::Override => h.handle.allow_anyway(),
            Op::UnloadAll => h.handle.unload_all(),
        }
        h.settle().await;
        let resolved_ok = collect(&mut pending, &mut live).await;

        // Invariant 3: nothing pending was admitted by a blocking step.
        if blocking_op {
            let status = h.status().await;
            for m in resolved_ok {
                let unloads_for_games = h.config.models[m].lifecycle.game_policy == GamePolicy::Unload;
                let blocked = status.paused || (status.session_active && !status.override_active && unloads_for_games);
                assert!(!blocked, "step {step} ({op:?}): model {} was admitted while blocked", MODELS[m]);
            }
        }
        if std::env::var_os("PROP_TRACE").is_some() {
            let st = h.status().await;
            let models: Vec<String> =
                st.models.iter().map(|m| format!("{}:{}/in{}/q{}", m.id, m.state, m.inflight, m.queued)).collect();
            let lives: Vec<String> = live
                .iter()
                .map(|(m, a)| format!("{}{}", MODELS[*m], if a.cancel.is_cancelled() { "(cancelled)" } else { "" }))
                .collect();
            eprintln!(
                "step {step} {op:?} => {models:?} live={lives:?} game={} paused={}",
                st.session_active, st.paused
            );
        }
        check_live(&h, &live, step, op).await;
    }

    // Quiesce: drop everything, end the game, resume, relieve pressure, wait.
    live.clear();
    h.handle.game_signal(GameSignal::inactive("process_list"));
    h.handle.set_paused(false);
    for _ in 0..15 {
        h.handle.vram_sample(VramSample { total_mib: 10_000, used_mib: 100, ..VramSample::default() });
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let late_ok = collect(&mut pending, &mut live).await;
    let _ = late_ok;

    // Invariant 4: every admit resolved.
    assert!(pending.is_empty(), "{} admit(s) never resolved", pending.len());

    // Release the late winners and let the system settle again.
    live.clear();
    tokio::time::sleep(Duration::from_secs(3600)).await;

    // Invariant 5: nothing stuck.
    let status = h.status().await;
    for m in &status.models {
        assert!(
            matches!(
                m.state,
                ModelStateKind::Unloaded | ModelStateKind::Ready | ModelStateKind::Held | ModelStateKind::Failed
            ),
            "model {} stuck in '{}' after the system went quiet",
            m.id,
            m.state.as_str()
        );
        assert_eq!(m.inflight, 0, "model {} still counts in-flight requests", m.id);
        assert_eq!(m.queued, 0, "model {} still has queued requests", m.id);
    }
}

/// How many requests of each model the test holds that are not cancelled.
fn uncancelled_counts(live: &[(usize, Admitted)]) -> Vec<usize> {
    (0..MODELS.len()).map(|i| live.iter().filter(|(m, a)| *m == i && !a.cancel.is_cancelled()).count()).collect()
}

/// Invariants 1 and 2 after a step.
///
/// The controller can handle a queued timer (a drain timeout that cancels
/// requests) between answering the status query and the test resuming, so the
/// snapshot and the test's cancel flags could describe different instants. The
/// flags are therefore read before and after the snapshot, and the comparison
/// is only made when they did not change in between.
async fn check_live(h: &Harness, live: &[(usize, Admitted)], step: usize, op: &Op) {
    let mut settled = None;
    for _ in 0..8 {
        let before = uncancelled_counts(live);
        let status = h.status().await;
        if before == uncancelled_counts(live) {
            settled = Some((status, before));
            break;
        }
    }
    let (status, uncancelled) = settled.expect("cancel flags kept changing while the controller was idle");
    for (i, name) in MODELS.iter().enumerate() {
        let reported = status.models.iter().find(|m| m.id == *name).map_or(0, |m| m.inflight);
        assert_eq!(
            reported, uncancelled[i],
            "step {step} ({op:?}): model {name} reports {reported} in-flight but the test holds {} live requests",
            uncancelled[i]
        );
        if uncancelled[i] > 0 {
            assert!(
                h.launcher.running(name),
                "step {step} ({op:?}): model {name} has {} live request(s) but its process is gone",
                uncancelled[i]
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn controller_invariants_hold_for_random_event_sequences(
        ops in proptest::collection::vec(op_strategy(), 1..40),
        policy in prop_oneof![Just(DrainPolicy::Finish), Just(DrainPolicy::WaitThenCancel), Just(DrainPolicy::Cancel)],
    ) {
        // A fresh single-threaded runtime with a paused clock per case, so
        // sleeps cost nothing and every run is deterministic.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .expect("runtime");
        rt.block_on(run_scenario(ops, policy));
    }
}

/// A sequence the property test once shrank to (admit, pressure unload, crash,
/// admit again). Kept as a plain test so a regression fails every run.
#[test]
fn shrunk_case_admit_pressure_crash_keeps_inflight_accounting_exact() {
    let ops = vec![
        Op::Admit(2),
        Op::Admit(0),
        Op::Admit(0),
        Op::Pressure(true),
        Op::Drop(4),
        Op::Admit(0),
        Op::Admit(0),
        Op::Pause(false),
        Op::Admit(2),
        Op::Drop(1),
        Op::Crash(0),
        Op::Admit(0),
        Op::Admit(0),
    ];
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().start_paused(true).build().expect("runtime");
    rt.block_on(run_scenario(ops, DrainPolicy::WaitThenCancel));
}
