//! Guards against drift between the Rust types the UI receives and the
//! hand-written TypeScript in `app/ui/src/lib/types.ts` (ADR-0012).
//!
//! Every JSON key produced by serialising the real Rust values must appear as a
//! property name in `types.ts`. Adding or renaming a Rust field without
//! updating the TypeScript fails this test.

use std::collections::BTreeSet;

use gatekeeper_core::config::Config;
use gatekeeper_core::config::schema::{GameExecutable, ModelConfig, ModelSource};
use gatekeeper_core::gate::controller::{ModelStatus, StatusSnapshot};
use gatekeeper_core::gate::model_state::{GateIndicator, ModelStateKind, UnloadCause};
use gatekeeper_core::gate::rejection::RejectKind;
use gatekeeper_core::gpu::{AdapterInfo, ProcessVram, VramSample};
use serde_json::Value;

/// The TypeScript source, read at test time.
fn types_ts() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/ui/src/lib/types.ts");
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"))
}

/// Collects every object key in a JSON value.
fn keys(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                out.insert(k.clone());
                keys(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| keys(i, out)),
        _ => {}
    }
}

/// True if `ts` declares a property called `key` (`key:` or `key?:`), or uses
/// it as a quoted string-literal property. The name must stand alone: `id:` is
/// not satisfied by `model_id:`.
fn declared(ts: &str, key: &str) -> bool {
    let standalone = |needle: &str| {
        ts.match_indices(needle).any(|(at, _)| {
            ts[..at].chars().next_back().is_none_or(|before| !(before.is_alphanumeric() || before == '_'))
        })
    };
    [format!("{key}:"), format!("{key}?:"), format!("'{key}':"), format!("\"{key}\":")]
        .iter()
        .any(|needle| standalone(needle))
}

fn assert_all_declared(what: &str, value: &Value, ts: &str) {
    let mut found = BTreeSet::new();
    keys(value, &mut found);
    // Free-form maps whose keys are user data, not fields.
    let user_data = ["ctx-size", "n-gpu-layers", "A"];
    let missing: Vec<&String> = found.iter().filter(|k| !user_data.contains(&k.as_str()) && !declared(ts, k)).collect();
    assert!(missing.is_empty(), "{what}: fields missing from app/ui/src/lib/types.ts: {missing:?}");
}

#[test]
fn config_fields_are_all_declared_in_typescript() {
    let ts = types_ts();
    let mut cfg = Config::default();
    let mut model = ModelConfig::default();
    model.source = Some(ModelSource::Huggingface {
        repo: "a/b".into(),
        revision: "main".into(),
        commit_sha: Some("x".into()),
        files: vec!["f.gguf".into()],
    });
    model.llama_args.insert("ctx-size".into(), 1.into());
    model.env.insert("A".into(), "b".into());
    cfg.models.push(model);
    cfg.games.executables.push(GameExecutable { name: "a.exe".into(), path: Some("p".into()), enabled: true });
    assert_all_declared("Config", &serde_json::to_value(&cfg).expect("json"), &ts);
}

/// Every serialized value of a string enum must appear as a quoted literal in
/// `types.ts`, so a new Rust variant cannot go unnoticed by the UI.
fn assert_values_declared<T: serde::Serialize>(what: &str, variants: &[T], ts: &str) {
    for v in variants {
        let name = serde_json::to_value(v).expect("json");
        let name = name.as_str().expect("string enum");
        assert!(ts.contains(&format!("'{name}'")), "{what}: value '{name}' is missing from app/ui/src/lib/types.ts");
    }
}

#[test]
fn enum_values_are_declared_in_typescript() {
    use ModelStateKind::{Draining, Failed, Held, Ready, Starting, Stopping, Unloaded};
    let ts = types_ts();
    assert_values_declared("ModelStateKind", &[Unloaded, Starting, Ready, Draining, Stopping, Held, Failed], &ts);
    assert_values_declared(
        "GateIndicator",
        &[
            GateIndicator::Paused,
            GateIndicator::Held,
            GateIndicator::Draining,
            GateIndicator::Closed,
            GateIndicator::Busy,
            GateIndicator::Open,
        ],
        &ts,
    );
    assert_values_declared(
        "RejectKind (blocked_reason)",
        &[RejectKind::GameRunning, RejectKind::Paused, RejectKind::VramGuard],
        &ts,
    );
    assert_values_declared(
        "UnloadCause",
        &[
            UnloadCause::Game,
            UnloadCause::Pause,
            UnloadCause::VramPressure,
            UnloadCause::Overflow,
            UnloadCause::IdleTtl,
            UnloadCause::MakeRoom,
            UnloadCause::User,
            UnloadCause::ConfigChange,
            UnloadCause::Shutdown,
        ],
        &ts,
    );
}

#[test]
fn status_and_event_fields_are_all_declared_in_typescript() {
    let ts = types_ts();
    let model = ModelStatus {
        id: "a".into(),
        display_name: "A".into(),
        state: ModelStateKind::Ready,
        cause: Some(UnloadCause::Game),
        blocked_reason: Some(RejectKind::GameRunning),
        inflight: 1,
        queued: 0,
        footprint_mib: Some(1),
        footprint_estimated: false,
        gpu_layers: Some((1, 2)),
        overflow: Some("x".into()),
        needs_restart: false,
        manual_hold: false,
        pid: Some(1),
        idle_seconds: 0,
        failure: Some("f".into()),
    };
    let vram = VramSample {
        total_mib: 1,
        used_mib: 1,
        shared_used_mib: 0,
        per_process: vec![ProcessVram { pid: 1, dedicated_mib: 1, shared_mib: 0 }],
    };
    let status = StatusSnapshot {
        gate: GateIndicator::Closed,
        paused: false,
        game: Some("g".into()),
        game_source: Some("s".into()),
        session_active: true,
        override_active: false,
        cooling_down: false,
        models: vec![model],
        vram: Some(vram),
    };
    assert_all_declared("StatusSnapshot", &serde_json::to_value(&status).expect("json"), &ts);

    let adapter = AdapterInfo {
        name: "n".into(),
        vendor_id: 1,
        luid: "l".into(),
        dedicated_mib: 1,
        software: false,
        integrated: false,
    };
    assert_all_declared("AdapterInfo", &serde_json::to_value(&adapter).expect("json"), &ts);
}

#[test]
fn startup_and_helper_types_are_declared_in_typescript() {
    use gatekeeper_core::app::{LaunchPreview, StartupInfo};
    use gatekeeper_core::detect::game_list::{GameCheck, RunningApp as RunningProgram};
    use gatekeeper_core::params::{ArgvWarning, ParamCatalog, ParamSpec, ValueKind};
    let ts = types_ts();
    let startup = StartupInfo::not_running(Some("e".into()));
    assert_all_declared("StartupInfo", &serde_json::to_value(startup).expect("json"), &ts);
    let preview = LaunchPreview {
        command: "c".into(),
        warnings: vec![ArgvWarning { key: "k".into(), message: "m".into() }],
        findings: vec![gatekeeper_core::config::ValidationIssue::warning("p", "m")],
    };
    assert_all_declared("LaunchPreview", &serde_json::to_value(preview).expect("json"), &ts);
    let spec = ParamSpec {
        key: "k".into(),
        flags: vec!["-k".into()],
        value_kind: ValueKind::Enum,
        placeholder: Some("N".into()),
        choices: Some(vec!["a".into()]),
        default: Some("a".into()),
        env: Some("E".into()),
        section: "s".into(),
        description: "d".into(),
        negation_flag: Some("--no-k".into()),
        repeatable: false,
        basic: true,
        reserved: false,
    };
    let catalog = ParamCatalog { engine_version: "v".into(), generated_at: "t".into(), params: vec![spec] };
    assert_all_declared("ParamCatalog", &serde_json::to_value(catalog).expect("json"), &ts);
    assert_values_declared(
        "ValueKind",
        &[
            ValueKind::Flag,
            ValueKind::Toggle,
            ValueKind::Enum,
            ValueKind::Integer,
            ValueKind::Float,
            ValueKind::String,
            ValueKind::Path,
            ValueKind::List,
        ],
        &ts,
    );
    let check =
        GameCheck { name: "n".into(), path: Some("p".into()), ok: true, error: Some("e".into()), warnings: vec![] };
    assert_all_declared("GameCheck", &serde_json::to_value(check).expect("json"), &ts);
    let program = RunningProgram { name: "n".into(), pid: 1, title: Some("t".into()), gpu_mib: 1 };
    assert_all_declared("RunningProgram", &serde_json::to_value(program).expect("json"), &ts);
}

fn sample_download() -> gatekeeper_core::hf::queue::DownloadInfo {
    use gatekeeper_core::hf::queue::{DownloadInfo, DownloadState};
    DownloadInfo {
        id: 1,
        label: "l".into(),
        repo: "a/b".into(),
        revision: "main".into(),
        commit_sha: Some("x".into()),
        state: DownloadState::Downloading,
        bytes_done: 1,
        bytes_total: 2,
        speed_bps: 3,
        current_file: Some("f".into()),
        error: Some("e".into()),
        needs_access: false,
        paths: vec!["p".into()],
        files: vec!["f".into()],
    }
}

#[test]
fn model_update_is_declared_in_typescript() {
    use gatekeeper_core::hf::update::{ModelUpdate, Verdict};
    let u = ModelUpdate {
        model_id: "m".into(),
        repo: "r".into(),
        verdict: Verdict::FilesChanged,
        latest_sha: Some("s".into()),
        message: "x".into(),
    };
    assert_all_declared("ModelUpdate", &serde_json::to_value(u).expect("json"), &types_ts());
}

#[test]
fn engine_update_types_are_declared_in_typescript() {
    use gatekeeper_core::app::EngineSwitchReport;
    use gatekeeper_core::engine::updater::{
        Backend, EngineAsset, EnginePlan, InstallProgress, InstalledEngine, UpdateCheck,
    };
    let ts = types_ts();
    let asset = EngineAsset { name: "n".into(), size: 1, url: "u".into(), sha256: Some("s".into()) };
    let plan = EnginePlan { tag: "b1".into(), backend: Backend::Cuda, assets: vec![asset] };
    assert_all_declared("EnginePlan", &serde_json::to_value(&plan).expect("json"), &ts);
    assert_all_declared("EngineAsset", &serde_json::to_value(&plan.assets[0]).expect("json"), &ts);
    let check = UpdateCheck {
        current_build: Some(1),
        latest_build: Some(2),
        newer: true,
        plan: Some(plan.clone()),
        alternatives: vec![plan],
    };
    assert_all_declared("UpdateCheck", &serde_json::to_value(check).expect("json"), &ts);
    let installed = InstalledEngine { name: "n".into(), path: "p".into() };
    assert_all_declared("InstalledEngine", &serde_json::to_value(installed).expect("json"), &ts);
    let progress = InstallProgress {
        step: "s".into(),
        bytes_done: 1,
        bytes_total: 2,
        finished: true,
        error: Some("e".into()),
        installed: Some("i".into()),
    };
    assert_all_declared("InstallProgress", &serde_json::to_value(progress).expect("json"), &ts);
    let report = EngineSwitchReport {
        engine_version: "v".into(),
        param_count: 1,
        unsupported: vec![("m".into(), "k".into(), "w".into())],
    };
    assert_all_declared("EngineSwitchReport", &serde_json::to_value(report).expect("json"), &ts);
}

#[test]
fn test_load_report_is_declared_in_typescript() {
    use gatekeeper_core::diagnostics::TestLoadReport;
    let report = TestLoadReport {
        ok: true,
        load_seconds: 1.0,
        gpu_layers: Some((1, 2)),
        footprint_mib: Some(3),
        footprint_estimated: false,
        overflow: Some("x".into()),
        message: "m".into(),
        speculative: vec![gatekeeper_core::speculative::SpecConfirmation {
            spec_type: "draft-simple".into(),
            confirmed: true,
        }],
    };
    assert_all_declared("TestLoadReport", &serde_json::to_value(report).expect("json"), &types_ts());
}

#[test]
fn download_and_library_types_are_declared_in_typescript() {
    use gatekeeper_core::hf::api::{RepoFile, RepoListing, RepoSummary};
    use gatekeeper_core::hf::library::LibraryFile;
    use gatekeeper_core::hf::view::build_view;
    let ts = types_ts();
    assert_all_declared("DownloadInfo", &serde_json::to_value(sample_download()).expect("json"), &ts);
    let repo = RepoSummary {
        id: "a/b".into(),
        author: "a".into(),
        downloads: 1,
        likes: 1,
        last_modified: Some("t".into()),
        gated: false,
    };
    assert_all_declared("RepoSummary", &serde_json::to_value(repo).expect("json"), &ts);
    let listing = RepoListing {
        sha: Some("s".into()),
        files: vec![
            RepoFile { path: "m-Q4_K_M.gguf".into(), size: 1, sha256: Some("h".into()) },
            RepoFile { path: "mmproj-f16.gguf".into(), size: 1, sha256: None },
            RepoFile { path: "mtp-m.gguf".into(), size: 1, sha256: None },
        ],
    };
    assert_all_declared("RepoView", &serde_json::to_value(build_view(&listing, Some(1024))).expect("json"), &ts);
    let file =
        LibraryFile { path: "p".into(), size: 1, quant: "Q4".into(), used_by: vec!["m".into()], is_projector: false };
    assert_all_declared("LibraryFile", &serde_json::to_value(file).expect("json"), &ts);
}

#[test]
fn speculative_types_are_declared_in_typescript() {
    use gatekeeper_core::config::ValidationIssue;
    use gatekeeper_core::hf::api::RepoSummary;
    use gatekeeper_core::hf::draft_search::DraftSuggestion;
    use gatekeeper_core::speculative::GgufSummary;
    let ts = types_ts();
    let suggestion = DraftSuggestion {
        repo: RepoSummary {
            id: "a/b".into(),
            author: "a".into(),
            downloads: 1,
            likes: 1,
            last_modified: None,
            gated: false,
        },
        reason: "r".into(),
    };
    assert_all_declared("DraftSuggestion", &serde_json::to_value(suggestion).expect("json"), &ts);
    let summary = GgufSummary {
        architecture: Some("qwen3".into()),
        size_bytes: 1,
        block_count: Some(1),
        vocab_size: Some(1),
        tokenizer_model: Some("gpt2".into()),
        nextn_predict_layers: Some(1),
        has_mtp_layers: true,
        detected_kind: Some("draft-mtp".into()),
    };
    assert_all_declared("GgufSummary", &serde_json::to_value(summary).expect("json"), &ts);
    // The severity names the page switches on.
    for (issue, name) in [
        (ValidationIssue::new("p", "m"), "error"),
        (ValidationIssue::warning("p", "m"), "warning"),
        (ValidationIssue::info("p", "m"), "info"),
    ] {
        assert_eq!(serde_json::to_value(&issue).expect("json")["severity"], name);
        assert!(ts.contains(&format!("'{name}'")), "severity '{name}' is missing from types.ts");
    }
}

#[test]
fn event_variants_are_declared_in_typescript() {
    use gatekeeper_core::engine::updater::InstallProgress;
    use gatekeeper_core::events::{AppEvent, NotificationKind};
    let ts = types_ts();
    let samples = vec![
        AppEvent::ModelState {
            model: "a".into(),
            from: ModelStateKind::Unloaded,
            to: ModelStateKind::Starting,
            cause: None,
        },
        AppEvent::Gate { paused: false, game: None, override_active: false },
        AppEvent::Vram(VramSample::default()),
        AppEvent::ModelLog { model: "a".into(), line: "l".into() },
        AppEvent::Notification { kind: NotificationKind::GameGate, title: "t".into(), body: "b".into() },
        AppEvent::Download(sample_download()),
        AppEvent::EngineInstall(InstallProgress::default()),
        AppEvent::ConfigReloaded,
        AppEvent::ConfigError { parse_error: None, issues: vec![] },
    ];
    for ev in samples {
        let v = serde_json::to_value(&ev).expect("json");
        let tag = v["type"].as_str().expect("tagged").to_owned();
        assert!(ts.contains(&format!("'{tag}'")), "event type '{tag}' is missing from the CoreEvent union");
        assert_all_declared(&format!("AppEvent::{tag}"), &v, &ts);
    }
}
