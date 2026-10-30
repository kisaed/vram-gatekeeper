//! End-to-end tests with real child processes (spec §22.2).
//!
//! The gatekeeper is started exactly as in production (real launcher, job
//! object, health probe, proxy, config watcher) but the "llama-server" is
//! this crate's `fake-llama-server` binary copied to `llama-server.exe`.
//! Only the process-list detector is scripted, because a real game is not
//! available to a test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::StreamExt;
use gatekeeper_core::app::{AppPaths, RunningApp, Services};
use gatekeeper_core::detect::process_scan::{ProcessLister, ProcessScanDetector};
use gatekeeper_core::events::AppEvent;
use serde_json::{Value, json};

/// Path of the fake server built by Cargo for this test run.
const FAKE: &str = env!("CARGO_BIN_EXE_fake-llama-server");

/// Process lister the test controls.
#[derive(Default)]
struct Scripted(Mutex<Vec<String>>);

impl ProcessLister for Scripted {
    fn running_names(&self) -> Vec<String> {
        self.0.lock().expect("lock").clone()
    }
}

/// A running gatekeeper over temp folders.
struct Rig {
    /// Temp root, deleted on drop.
    _root: tempfile::TempDir,
    /// Config folder.
    config_dir: PathBuf,
    /// Engine folder containing `llama-server.exe`.
    engine_dir: PathBuf,
    /// The app under test.
    app: Option<RunningApp>,
    /// Scripted process list.
    procs: Arc<Scripted>,
    /// HTTP client.
    http: reqwest::Client,
}

/// Base config as JSON; tests tweak it before starting.
fn base_config(engine_dir: &Path, model_file: &Path, env: Value) -> Value {
    json!({
        "server": { "port": 0 },
        "engine": { "llama_server_dir": engine_dir.to_string_lossy() },
        "games": {
            "executables": ["fake-game.exe"],
            "scan_interval_seconds": 1,
            "cooldown_seconds": 1
        },
        "draining": { "policy": "cancel" },
        "vram": { "loop_guard": { "enabled": true } },
        "models": [{
            "id": "fake",
            "files": { "model": model_file.to_string_lossy() },
            "lifecycle": { "load_timeout_seconds": 30 },
            "llama_args": { "ctx-size": 1024 },
            "env": env
        }]
    })
}

impl Rig {
    /// Starts the app with `config` (a JSON document) in a fresh temp tree.
    async fn start(env: Value, tweak: impl FnOnce(&mut Value)) -> Rig {
        let root = tempfile::tempdir().expect("tempdir");
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        let engine_dir = root.path().join("engine");
        std::fs::create_dir_all(&config_dir).expect("mkdir");
        std::fs::create_dir_all(&engine_dir).expect("mkdir");
        std::fs::copy(FAKE, engine_dir.join("llama-server.exe")).expect("copy fake server");
        let model_file = root.path().join("model.gguf");
        std::fs::write(&model_file, vec![0u8; 1024]).expect("model file");

        let mut cfg = base_config(&engine_dir, &model_file, env);
        tweak(&mut cfg);
        std::fs::write(config_dir.join("config.json"), serde_json::to_string_pretty(&cfg).expect("json"))
            .expect("write config");

        let procs = Arc::new(Scripted::default());
        let mut services = Services::default();
        services.detectors.push(Box::new(ProcessScanDetector::new(procs.clone())));
        let paths = AppPaths { config_dir: config_dir.clone(), data_dir, bundled_engine_dir: None };
        let app = RunningApp::start(paths, services).await.expect("app starts");
        Rig { _root: root, config_dir, engine_dir, app: Some(app), procs, http: reqwest::Client::new() }
    }

    fn app(&self) -> &RunningApp {
        self.app.as_ref().expect("running")
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.app().proxy_addr)
    }

    fn chat(&self, stream: bool) -> reqwest::RequestBuilder {
        self.http
            .post(self.url("/v1/chat/completions"))
            .json(&json!({ "model": "fake", "stream": stream, "messages": [{ "role": "user", "content": "hi" }] }))
    }

    async fn model_state(&self) -> String {
        self.app().controller.status().await.expect("status").models[0].state.as_str().to_owned()
    }

    async fn wait_state(&self, want: &str) {
        for _ in 0..200 {
            if self.model_state().await == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("model never reached state '{want}', is '{}'", self.model_state().await);
    }

    fn game(&self, running: bool) {
        let mut p = self.procs.0.lock().expect("lock");
        p.retain(|n| n != "fake-game.exe");
        if running {
            p.push("fake-game.exe".into());
        }
    }

    async fn finish(mut self) {
        if let Some(app) = self.app.take() {
            app.shutdown().await;
        }
    }
}

/// Windows process liveness via `tasklist`.
fn pid_alive(pid: u32) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
}

async fn read_stream(resp: reqwest::Response) -> String {
    let mut out = String::new();
    let mut s = resp.bytes_stream();
    while let Some(Ok(b)) = s.next().await {
        out.push_str(&String::from_utf8_lossy(&b));
    }
    out
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn gate_works_end_to_end_with_real_processes() {
    let rig = Rig::start(json!({ "FAKE_LOAD_MS": "200", "FAKE_TOKENS": "40", "FAKE_TOKEN_MS": "50" }), |_| {}).await;

    // 1. A request loads the model on demand and gets a real answer.
    assert_eq!(rig.model_state().await, "unloaded");
    let resp = rig.chat(false).send().await.expect("send");
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.expect("json");
    assert!(body["choices"][0]["message"]["content"].as_str().expect("content").contains("t0 t1"));
    assert_eq!(rig.model_state().await, "ready");
    let pid = rig.app().controller.status().await.expect("status").models[0].pid.expect("pid");
    assert!(pid_alive(pid));

    // The child's output reached the log ring buffer.
    assert!(rig.app().logs.tail("fake", 50).iter().any(|l| l.contains("chat request received")));

    // 2. A game starts mid-stream; policy `cancel` ends the stream with an
    //    in-stream error and the process is killed (FR-GAME-3, FR-GAME-4).
    let stream = rig.chat(true).send().await.expect("send");
    let reader = tokio::spawn(read_stream(stream));
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.game(true);
    let text = reader.await.expect("join");
    assert!(text.contains("request_cancelled"), "{text}");
    assert!(!text.contains("[DONE]"));
    rig.wait_state("unloaded").await;
    for _ in 0..40 {
        if !pid_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!pid_alive(pid), "llama-server process must be gone so VRAM is freed");

    // 3. While the game runs, requests get the structured error (FR-GAME-5).
    let gated = rig.chat(false).send().await.expect("send");
    assert_eq!(gated.status(), 503);
    let v: Value = gated.json().await.expect("json");
    assert_eq!(v["error"]["code"], "game_running");
    assert_eq!(v["error"]["gatekeeper"]["detail"]["game"], "fake-game.exe");

    // 4. After the game exits and the cooldown passes, it loads again (FR-GAME-6).
    rig.game(false);
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let again = rig.chat(false).send().await.expect("send");
    assert_eq!(again.status(), 200);
    let new_pid = rig.app().controller.status().await.expect("status").models[0].pid.expect("pid");
    assert_ne!(new_pid, pid);

    rig.finish().await;
    for _ in 0..40 {
        if !pid_alive(new_pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("shutdown must kill the child");
}

#[tokio::test]
async fn partial_layer_offload_in_real_output_is_treated_as_overflow() {
    let rig = Rig::start(json!({ "FAKE_LAYERS": "20/33", "FAKE_LOAD_MS": "800" }), |_| {}).await;
    let resp = rig.chat(false).send().await.expect("send");
    assert_eq!(resp.status(), 503, "FR-VRAM-5: the triggering request gets an error");
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["code"], "request_cancelled");
    assert_eq!(v["error"]["gatekeeper"]["detail"]["cause"], "overflow");
    rig.wait_state("unloaded").await;
    rig.finish().await;
}

#[tokio::test]
async fn explicit_gpu_layer_count_keeps_a_partial_offload_running() {
    let rig = Rig::start(json!({ "FAKE_LAYERS": "20/33" }), |cfg| {
        cfg["models"][0]["llama_args"]["n-gpu-layers"] = json!(20);
    })
    .await;
    assert_eq!(rig.chat(false).send().await.expect("send").status(), 200);
    let status = rig.app().controller.status().await.expect("status");
    assert_eq!(status.models[0].gpu_layers, Some((20, 33)));
    rig.finish().await;
}

#[tokio::test]
async fn startup_failure_is_reported_with_the_childs_message() {
    let rig = Rig::start(json!({ "FAKE_FAIL_START": "unknown argument: --bogus" }), |_| {}).await;
    let resp = rig.chat(false).send().await.expect("send");
    assert_eq!(resp.status(), 503);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["code"], "model_load_failed");
    assert!(v["error"]["message"].as_str().expect("msg").contains("exited during startup"));
    assert!(v["error"]["gatekeeper"]["detail"]["log_tail"].to_string().contains("--bogus"));
    rig.finish().await;
}

#[tokio::test]
async fn a_port_collision_is_retried_transparently() {
    let marker = std::env::temp_dir().join(format!("fake-bind-marker-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let rig = Rig::start(json!({ "FAKE_BIND_FAIL_ONCE": marker.to_string_lossy() }), |_| {}).await;
    let started = std::time::Instant::now();
    let resp = rig.chat(false).send().await.expect("send");
    assert_eq!(resp.status(), 200, "second attempt on a new port succeeds");
    assert!(marker.exists(), "the first attempt really failed");
    assert!(started.elapsed() < Duration::from_secs(8), "no 10 s failure backoff was involved");
    let _ = std::fs::remove_file(&marker);
    rig.finish().await;
}

#[tokio::test]
async fn crash_while_serving_is_recovered_by_the_next_request() {
    let rig = Rig::start(json!({ "FAKE_CRASH_AFTER_MS": "1500", "FAKE_LOAD_MS": "100" }), |_| {}).await;
    assert_eq!(rig.chat(false).send().await.expect("send").status(), 200);
    rig.wait_state("unloaded").await; // crashed and cleaned up
    let again = rig.chat(false).send().await.expect("send");
    assert_eq!(again.status(), 200, "next request starts it again");
    rig.finish().await;
}

#[tokio::test]
async fn parameter_catalog_is_generated_from_the_engines_help() {
    let rig = Rig::start(json!({}), |_| {}).await;
    let catalog = rig.app().catalog();
    assert!(catalog.engine_version.contains("9999"), "version read from --version: {}", catalog.engine_version);
    let ctx = catalog.resolve("ctx-size").expect("ctx-size");
    assert_eq!(ctx.default.as_deref(), Some("0"));
    assert!(catalog.resolve("n-gpu-layers").is_some());
    assert!(catalog.resolve("port").expect("port").reserved);
    assert!(catalog.params.len() > 25, "parsed {} params", catalog.params.len());
    rig.finish().await;
}

#[tokio::test]
async fn hand_edits_apply_live_and_invalid_edits_are_rejected() {
    let rig = Rig::start(json!({}), |_| {}).await;
    let mut events = rig.app().events.subscribe();

    // A valid edit adds a second model.
    let path = rig.config_dir.join("config.json");
    let mut cfg: Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    let mut second = cfg["models"][0].clone();
    second["id"] = json!("second");
    cfg["models"].as_array_mut().expect("array").push(second);
    std::fs::write(&path, serde_json::to_string_pretty(&cfg).expect("json")).expect("write");

    let mut seen = false;
    for _ in 0..60 {
        let v: Value = rig.http.get(rig.url("/v1/models")).send().await.expect("send").json().await.expect("json");
        if v["data"].as_array().expect("array").len() == 2 {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(seen, "valid edit applied without restart");

    // An invalid edit is rejected: the old config stays and the error is published.
    std::fs::write(&path, r#"{ "vram": { "pressure": { "threshold_percent": 0 } } }"#).expect("write");
    let mut rejected = false;
    for _ in 0..60 {
        if let Ok(Ok(AppEvent::ConfigError { issues, .. })) =
            tokio::time::timeout(Duration::from_millis(200), events.recv()).await
        {
            assert_eq!(issues[0].path, "vram.pressure.threshold_percent");
            rejected = true;
            break;
        }
    }
    assert!(rejected, "invalid edit reported with its field path");
    let v: Value = rig.http.get(rig.url("/v1/models")).send().await.expect("send").json().await.expect("json");
    assert_eq!(v["data"].as_array().expect("array").len(), 2, "previous config still in effect");
    assert!(
        std::fs::read_to_string(&path).expect("read").contains("threshold_percent"),
        "bad file left for the user to fix"
    );
    let _ = &rig.engine_dir;
    rig.finish().await;
}

// ---- job object: children die with the parent (NFR-5) ---------------------

/// Runs in a child copy of this test binary: starts a model and prints the
/// llama-server pid, then waits to be killed by the parent test.
#[tokio::test]
async fn helper_hold_a_model_open() {
    if std::env::var("E2E_HELPER").is_err() {
        return; // only meaningful when launched by `children_die_when_the_parent_is_killed`
    }
    let rig = Rig::start(json!({ "FAKE_LOAD_MS": "100" }), |_| {}).await;
    assert_eq!(rig.chat(false).send().await.expect("send").status(), 200);
    let pid = rig.app().controller.status().await.expect("status").models[0].pid.expect("pid");
    println!("CHILD_PID={pid}");
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[test]
fn children_die_when_the_parent_is_killed() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().expect("exe");
    let mut parent = Command::new(exe)
        .args(["helper_hold_a_model_open", "--exact", "--nocapture", "--test-threads=1"])
        .env("E2E_HELPER", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn helper");

    let mut child_pid = None;
    let reader = BufReader::new(parent.stdout.take().expect("stdout"));
    for line in reader.lines().map_while(Result::ok) {
        // libtest prints "test <name> ... " on the same line before our output.
        if let Some((_, p)) = line.split_once("CHILD_PID=") {
            child_pid = p.trim().parse::<u32>().ok();
            break;
        }
    }
    let child_pid = child_pid.expect("helper reported the llama-server pid");
    assert!(pid_alive(child_pid), "model is running while its parent lives");

    // Simulate a crash / Task Manager kill: no chance to clean up.
    parent.kill().expect("kill parent");
    let _ = parent.wait();

    for _ in 0..50 {
        if !pid_alive(child_pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("llama-server {child_pid} survived its parent: the job object is not killing children");
}
