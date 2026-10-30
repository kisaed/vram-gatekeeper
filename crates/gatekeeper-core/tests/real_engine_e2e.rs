//! Phase 1 acceptance with the real llama.cpp engine (spec §26 Phase 1).
//!
//! Runs only when both variables are set, so machines without an engine skip it:
//!
//! ```text
//! GATEKEEPER_REAL_ENGINE=C:\path\to\engine\cuda     (folder with llama-server.exe)
//! GATEKEEPER_REAL_MODEL=C:\path\to\small-model.gguf
//! ```
//!
//! A real process, a copy of `ping.exe` renamed `fakegame.exe`, stands in for a
//! game. It is found by the real Toolhelp process scanner, exactly as a game
//! would be.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use gatekeeper_core::app::{AppPaths, RunningApp, Services};
use gatekeeper_core::detect::process_scan::{ProcessScanDetector, WindowsProcessLister};
use gatekeeper_core::gate::model_state::ModelStateKind;
use gatekeeper_core::gpu::windows_monitor::WindowsGpuMonitor;
use serde_json::{Value, json};

/// Real engine and model paths, if configured.
fn real_inputs() -> Option<(PathBuf, PathBuf)> {
    let engine = std::env::var_os("GATEKEEPER_REAL_ENGINE").map(PathBuf::from)?;
    let model = std::env::var_os("GATEKEEPER_REAL_MODEL").map(PathBuf::from)?;
    (engine.join("llama-server.exe").is_file() && model.is_file()).then_some((engine, model))
}

struct Rig {
    _root: tempfile::TempDir,
    root_path: PathBuf,
    app: Option<RunningApp>,
    http: reqwest::Client,
}

impl Rig {
    async fn start(engine: &Path, model: &Path, llama_args: Value) -> Rig {
        Self::start_with(engine, model, llama_args, false).await
    }

    async fn start_with(engine: &Path, model: &Path, llama_args: Value, with_gpu: bool) -> Rig {
        Self::start_full(engine, model, llama_args, with_gpu, None).await
    }

    /// Like `start_with`, with an optional draft file (`files.draft`).
    async fn start_full(engine: &Path, model: &Path, llama_args: Value, with_gpu: bool, draft: Option<&Path>) -> Rig {
        let root = tempfile::tempdir().expect("tempdir");
        let config_dir = root.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        let cfg = json!({
            "server": { "port": 0 },
            "engine": { "llama_server_dir": engine.to_string_lossy() },
            "games": { "executables": ["fakegame.exe"], "scan_interval_seconds": 1, "cooldown_seconds": 2 },
            "draining": { "policy": "cancel" },
            "models": [{
                "id": "real",
                "files": { "model": model.to_string_lossy(), "draft": draft.map(|d| d.to_string_lossy().into_owned()) },
                "lifecycle": { "load_timeout_seconds": 120 },
                "llama_args": llama_args
            }]
        });
        std::fs::write(config_dir.join("config.json"), cfg.to_string()).expect("write config");
        let mut services = Services::default();
        services.detectors.push(Box::new(ProcessScanDetector::new(Arc::new(WindowsProcessLister))));
        if with_gpu {
            services.gpu = Some(Arc::new(WindowsGpuMonitor::new(None).expect("GPU counters")));
        }
        let paths = AppPaths { config_dir, data_dir: root.path().join("data"), bundled_engine_dir: None };
        let app = RunningApp::start(paths, services).await.expect("app starts");
        Rig { root_path: root.path().to_path_buf(), _root: root, app: Some(app), http: reqwest::Client::new() }
    }

    fn app(&self) -> &RunningApp {
        self.app.as_ref().expect("running")
    }

    fn chat(&self, stream: bool, max_tokens: u32) -> reqwest::RequestBuilder {
        self.http.post(format!("http://{}/v1/chat/completions", self.app().proxy_addr)).json(&json!({
            "model": "real",
            "stream": stream,
            "max_tokens": max_tokens,
            // A tiny model sometimes ends its answer after a dozen tokens, which
            // would finish the stream before the "game" starts. Keep it talking.
            "ignore_eos": true,
            "messages": [{ "role": "user", "content": "Count from 1 to 500, separated by commas." }]
        }))
    }

    /// Starts the stand-in game: a renamed copy of ping.exe that runs for a while.
    fn launch_game(&self) -> Child {
        let exe = self.root_path.join("fakegame.exe");
        if !exe.exists() {
            std::fs::copy(r"C:\Windows\System32\PING.EXE", &exe).expect("copy ping");
        }
        Command::new(&exe).args(["-n", "300", "127.0.0.1"]).stdout(Stdio::null()).spawn().expect("start fake game")
    }

    async fn model(&self) -> gatekeeper_core::gate::controller::ModelStatus {
        self.app().controller.status().await.expect("status").models.remove(0)
    }

    async fn wait_state(&self, want: &str) {
        for _ in 0..300 {
            if self.model().await.state.as_str() == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("model never reached '{want}', is '{}'", self.model().await.state.as_str());
    }
}

/// True while a process id is alive.
fn pid_alive(pid: u32) -> bool {
    let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

#[tokio::test]
async fn real_model_is_gated_by_a_real_process_and_comes_back() {
    let Some((engine, model)) = real_inputs() else {
        eprintln!("skipped: set GATEKEEPER_REAL_ENGINE and GATEKEEPER_REAL_MODEL to run");
        return;
    };
    let rig = Rig::start(&engine, &model, json!({ "ctx-size": 4096, "n-gpu-layers": "auto" })).await;

    // A request loads the real model on demand and gets real text.
    let resp = rig.chat(false, 16).send().await.expect("send");
    assert_eq!(resp.status(), 200, "real model answers");
    let body: Value = resp.json().await.expect("json");
    assert!(!body["choices"][0]["message"]["content"].as_str().unwrap_or("").is_empty());
    let m = rig.model().await;
    assert_eq!(m.state, ModelStateKind::Ready);
    let (off, total) = m.gpu_layers.expect("layer placement parsed from the real log");
    assert_eq!(off, total, "fully on the GPU, so no overflow");
    assert!(m.overflow.is_none());
    let pid = m.pid.expect("pid");
    assert!(pid_alive(pid));

    // A long streaming request, then the "game" starts mid-stream.
    let stream = rig.chat(true, 3000).send().await.expect("send");
    let reader = tokio::spawn(async move {
        let mut text = String::new();
        let mut s = stream.bytes_stream();
        while let Some(Ok(b)) = s.next().await {
            text.push_str(&String::from_utf8_lossy(&b));
        }
        text
    });
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut game = rig.launch_game();

    let text = reader.await.expect("join");
    assert!(
        text.contains("request_cancelled"),
        "stream ended with the in-stream error: {}",
        &text[text.len().saturating_sub(300)..]
    );
    assert!(!text.contains("[DONE]"));
    rig.wait_state("unloaded").await;
    for _ in 0..50 {
        if !pid_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!pid_alive(pid), "llama-server is gone, so its VRAM is released");

    // While the game runs, requests are refused with the structured error.
    let gated = rig.chat(false, 8).send().await.expect("send");
    assert_eq!(gated.status(), 503);
    let v: Value = gated.json().await.expect("json");
    assert_eq!(v["error"]["code"], "game_running");
    assert!(v["error"]["gatekeeper"]["detail"]["game"].as_str().unwrap_or("").eq_ignore_ascii_case("fakegame.exe"));

    // The game exits; after the cooldown the model loads again.
    game.kill().expect("kill game");
    let _ = game.wait();
    tokio::time::sleep(Duration::from_secs(5)).await;
    let again = rig.chat(false, 8).send().await.expect("send");
    assert_eq!(again.status(), 200, "model reloaded on demand after the game closed");

    let new_pid = rig.model().await.pid.expect("pid");
    rig.app.as_ref().expect("app");
    let mut rig = rig;
    rig.app.take().expect("app").shutdown().await;
    for _ in 0..50 {
        if !pid_alive(new_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("shutdown must kill llama-server");
}

#[tokio::test]
async fn real_fit_moving_layers_to_the_cpu_is_caught_as_overflow() {
    let Some((engine, model)) = real_inputs() else {
        eprintln!("skipped: set GATEKEEPER_REAL_ENGINE and GATEKEEPER_REAL_MODEL to run");
        return;
    };
    // Demanding more free VRAM than exists forces llama.cpp's --fit to shed
    // every layer to the CPU, the same thing it does when a game fills VRAM.
    let rig = Rig::start(&engine, &model, json!({ "ctx-size": 2048, "fit": "on", "fit-target": "31000" })).await;
    let resp = rig.chat(false, 8).send().await.expect("send");
    assert_eq!(resp.status(), 503, "FR-VRAM-5: the request that triggered the overflow gets an error");
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["code"], "request_cancelled");
    assert_eq!(v["error"]["gatekeeper"]["detail"]["cause"], "overflow");
    rig.wait_state("unloaded").await;
    let mut rig = rig;
    rig.app.take().expect("app").shutdown().await;
}

#[tokio::test]
async fn real_gpu_counters_attribute_the_model_and_show_vram_released_after_unload() {
    let Some((engine, model)) = real_inputs() else {
        eprintln!("skipped: set GATEKEEPER_REAL_ENGINE and GATEKEEPER_REAL_MODEL to run");
        return;
    };
    // A large context makes the KV cache big enough to measure clearly.
    let rig = Rig::start_with(&engine, &model, json!({ "ctx-size": 65536, "parallel": 1 }), true).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    let used_before = rig.app().controller.status().await.expect("status").vram.expect("idle sample").used_mib;

    assert_eq!(rig.chat(false, 8).send().await.expect("send").status(), 200);
    // Footprint is read 5 s after ready (spec §9.5); samples arrive each second.
    tokio::time::sleep(Duration::from_secs(8)).await;
    let st = rig.app().controller.status().await.expect("status");
    let m = &st.models[0];
    let used_loaded = st.vram.as_ref().expect("sample").used_mib;
    let footprint = m.footprint_mib.expect("footprint");
    eprintln!(
        "adapter used before {used_before} MiB, with model {used_loaded} MiB (+{}), per-process footprint {footprint} MiB (estimated={})",
        used_loaded.saturating_sub(used_before),
        m.footprint_estimated
    );
    assert!(!m.footprint_estimated, "footprint was measured from the per-process counter");
    assert!((300..8000).contains(&footprint), "plausible footprint {footprint}");
    let grew = used_loaded.saturating_sub(used_before);
    assert!(grew.abs_diff(footprint) < 800, "adapter growth {grew} MiB roughly matches the footprint {footprint} MiB");
    assert_eq!(m.overflow, None, "no spill at this size");

    // Unload and time how long until the driver has released the memory.
    let t0 = std::time::Instant::now();
    rig.app().controller.unload("real", false).await;
    rig.wait_state("unloaded").await;
    let stopped_after = t0.elapsed();
    tokio::time::sleep(Duration::from_secs(2)).await;
    let used_after = rig.app().controller.status().await.expect("status").vram.expect("sample").used_mib;
    eprintln!(
        "unload took {stopped_after:?} until `unloaded`; adapter used afterwards {used_after} MiB (before: {used_before} MiB)"
    );
    assert!(
        used_after.abs_diff(used_before) < 600,
        "VRAM returned to baseline: before {used_before}, after {used_after}"
    );
    assert!(stopped_after < Duration::from_secs(5), "release wait ends promptly once usage drops");
    let mut rig = rig;
    rig.app.take().expect("app").shutdown().await;
}

// ------------------------------------------------- speculative decoding

/// Starts the model with `spec_type` (and itself as its draft when `self_draft`), answers one chat request, and
/// returns the Test Load report.
async fn speculative_run(spec_type: &str, self_draft: bool) -> Option<gatekeeper_core::diagnostics::TestLoadReport> {
    let (engine, model) = real_inputs().or_else(|| {
        eprintln!("skipped: set GATEKEEPER_REAL_ENGINE and GATEKEEPER_REAL_MODEL to run");
        None
    })?;
    let mut rig = Rig::start_full(
        &engine,
        &model,
        json!({ "ctx-size": 4096, "spec-type": spec_type }),
        false,
        self_draft.then_some(model.as_path()),
    )
    .await;
    let report = rig.app().test_load("real").await;
    if report.ok {
        let resp = rig.chat(false, 16).send().await.expect("send");
        assert_eq!(resp.status(), 200, "the model answers with speculative decoding on");
        let body: Value = resp.json().await.expect("json");
        assert!(!body["choices"][0]["message"]["content"].as_str().unwrap_or("").is_empty());
    }
    rig.app.take().expect("app").shutdown().await;
    Some(report)
}

#[tokio::test]
async fn real_engine_loads_with_the_model_as_its_own_draft() {
    // The same file has the same vocabulary, so the engine accepts it as a draft.
    let Some(report) = speculative_run("draft-simple", true).await else { return };
    assert!(report.ok, "{}", report.message);
    assert_eq!(report.speculative.len(), 1);
    assert_eq!(report.speculative[0].spec_type, "draft-simple");
    assert!(report.speculative[0].confirmed, "the engine reported starting draft-simple: {report:?}");
}

#[tokio::test]
async fn real_engine_loads_with_ngram_mod() {
    let Some(report) = speculative_run("ngram-mod", false).await else { return };
    assert!(report.ok, "{}", report.message);
    assert!(report.speculative.iter().all(|s| s.confirmed), "{report:?}");
}

#[tokio::test]
async fn real_engine_reports_a_model_without_prediction_layers_that_is_asked_for_mtp() {
    let Some(report) = speculative_run("draft-mtp", false).await else { return };
    eprintln!(
        "MTP on a plain model: ok={} message={:?} speculative={:?}",
        report.ok, report.message, report.speculative
    );
    // Either the engine refuses to start (the report fails with its message) or it
    // continues without MTP (the type is then not confirmed). It must never claim success for MTP.
    assert!(!report.ok || report.speculative.iter().all(|s| !s.confirmed), "{report:?}");
}

#[test]
fn real_gguf_files_are_summarised_and_pass_the_speculative_checks() {
    let Some((_, model)) = real_inputs() else {
        eprintln!("skipped: set GATEKEEPER_REAL_ENGINE and GATEKEEPER_REAL_MODEL to run");
        return;
    };
    let s = gatekeeper_core::speculative::summarize(&model).expect("a real GGUF file is read");
    eprintln!("{s:?}");
    assert!(s.architecture.is_some() && s.block_count.is_some_and(|n| n > 0));
    assert!(s.vocab_size.is_some_and(|n| n > 1000), "the vocabulary is counted: {s:?}");
    assert!(s.tokenizer_model.is_some());
    assert!(!s.has_mtp_layers && s.detected_kind.is_none(), "an ordinary model is not recognised as a draft type");
    let meta = gatekeeper_core::gguf::header::read_meta_cached(&model).expect("meta");
    assert!(meta.has_tensor("token_embd.weight"), "tensor names come from the real tensor table");

    // The model as its own draft has identical vocabularies: no findings at all.
    let mut m = gatekeeper_core::config::schema::ModelConfig { id: "real".into(), ..Default::default() };
    m.files.model = model.to_string_lossy().into_owned();
    m.files.draft = Some(model.to_string_lossy().into_owned());
    m.llama_args.insert("spec-type".into(), "draft-simple".into());
    let found = gatekeeper_core::speculative::findings(&m, "models[0]", None, true);
    assert!(found.is_empty(), "{found:?}");
}
