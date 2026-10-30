//! Tests for application start-up safety rules (spec §19, §21).

use std::sync::Arc;

use serde_json::json;

use crate::app::{AppPaths, RunningApp, SECRET_LAN_API_KEY, Services};
use crate::gate::model_state::ModelStateKind;
use crate::gate::test_support::FakeLauncher;
use crate::ports::{MemorySecretStore, SecretStore};

/// Starts an app over a temp tree with `config` as `config.json`.
async fn start(config: serde_json::Value, secrets: Arc<MemorySecretStore>) -> (tempfile::TempDir, RunningApp) {
    let root = tempfile::tempdir().expect("tempdir");
    let config_dir = root.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(config_dir.join("config.json"), config.to_string()).expect("write config");
    let paths = AppPaths { config_dir, data_dir: root.path().join("data"), bundled_engine_dir: None };
    let services =
        Services { launcher: Some(Arc::new(FakeLauncher::default())), secrets: Some(secrets), ..Services::default() };
    let app = RunningApp::start(paths, services).await.expect("app starts");
    (root, app)
}

#[tokio::test]
async fn lan_mode_without_a_stored_key_falls_back_to_localhost_only() {
    // The file claims a key exists, but the secret store is empty: the claim
    // must not be trusted (anyone can hand-edit `api_key_set`).
    let cfg = json!({ "server": { "port": 0, "allow_lan": true, "api_key_set": true } });
    let (_root, app) = start(cfg, Arc::new(MemorySecretStore::default())).await;
    assert!(app.proxy_addr.ip().is_loopback(), "listening on {}", app.proxy_addr);
    assert!(!app.lan_bound);
    assert!(app.startup.issues.iter().any(|i| i.path == "server.allow_lan"), "the user is told why");
    app.shutdown().await;
}

#[tokio::test]
async fn hand_edited_wildcard_host_without_a_key_is_forced_to_loopback() {
    let cfg = json!({ "server": { "port": 0, "host": "0.0.0.0", "api_key_set": true } });
    let (_root, app) = start(cfg, Arc::new(MemorySecretStore::default())).await;
    assert!(app.proxy_addr.ip().is_loopback());
    app.shutdown().await;
}

#[tokio::test]
async fn lan_mode_with_a_stored_key_listens_on_the_network_and_demands_the_key() {
    let secrets = Arc::new(MemorySecretStore::default());
    secrets.set(SECRET_LAN_API_KEY, "s3cret").expect("set");
    let cfg = json!({ "server": { "port": 0, "allow_lan": true, "api_key_set": true } });
    let (_root, app) = start(cfg, secrets).await;
    assert!(app.lan_bound);
    assert!(app.proxy_addr.ip().is_unspecified(), "listening on {}", app.proxy_addr);

    let url = format!("http://127.0.0.1:{}/v1/models", app.proxy_addr.port());
    let http = reqwest::Client::new();
    assert_eq!(http.get(&url).send().await.expect("send").status(), 401);
    assert_eq!(http.get(&url).bearer_auth("s3cret").send().await.expect("send").status(), 200);

    // The key cannot be removed while the network is exposed.
    assert!(app.set_api_key(None).is_err());
    assert_eq!(http.get(&url).bearer_auth("s3cret").send().await.expect("send").status(), 200);
    app.shutdown().await;
}

#[tokio::test]
async fn loopback_mode_needs_no_key_and_allows_clearing_one() {
    let (_root, app) = start(json!({ "server": { "port": 0 } }), Arc::new(MemorySecretStore::default())).await;
    assert!(!app.lan_bound);
    let url = format!("http://127.0.0.1:{}/v1/models", app.proxy_addr.port());
    assert_eq!(reqwest::get(&url).await.expect("send").status(), 200);
    app.set_api_key(Some("k")).expect("set");
    assert_eq!(reqwest::get(&url).await.expect("send").status(), 401);
    app.set_api_key(None).expect("clear");
    assert_eq!(reqwest::get(&url).await.expect("send").status(), 200);
    app.shutdown().await;
}

/// The host shares `RunningApp` across Tauri commands, so it must be `Send + Sync`.
#[test]
fn running_app_can_be_shared_across_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RunningApp>();
}

/// Search, listing, download, model creation and library through the running
/// app, against the stub Hub.
#[tokio::test]
async fn downloads_become_models_through_the_running_app() {
    use crate::hf::api::SearchSort;
    use crate::hf::hf_tests::{content, sha, start_stub};
    use crate::hf::queue::{DownloadSpec, DownloadState};

    let data = content(1);
    let (hub, _stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    let root = tempfile::tempdir().expect("tempdir");
    let models = root.path().join("models");
    let config_dir = root.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(
        config_dir.join("config.json"),
        serde_json::json!({ "server": { "port": 0 }, "downloads": { "models_dir": models.to_string_lossy() } })
            .to_string(),
    )
    .expect("config");
    let paths = AppPaths { config_dir, data_dir: root.path().join("data"), bundled_engine_dir: None };
    let services =
        Services { launcher: Some(Arc::new(FakeLauncher::default())), hf_base_url: Some(hub), ..Services::default() };
    let app = RunningApp::start(paths, services).await.expect("app starts");

    assert_eq!(app.hf.search("model", SearchSort::Downloads, 10).await.expect("search").len(), 2);
    let view = app.repo_view("org/Model-GGUF", "main").await.expect("repo view");
    assert_eq!(view.sha.as_deref(), Some("deadbeefcafe"));
    assert_eq!(view.variants[0].variant.quant, "Q4_K_M");

    let id = app
        .downloads
        .enqueue(DownloadSpec {
            repo: "org/Model-GGUF".into(),
            revision: "main".into(),
            commit_sha: view.sha.clone(),
            files: view.variants[0].variant.files.clone(),
            label: "Test model (Q4_K_M)".into(),
        })
        .await
        .expect("enqueue");
    let mut finished = None;
    for _ in 0..100 {
        if let Some(d) = app.downloads.list().await.into_iter().find(|d| d.id == id && d.state == DownloadState::Done) {
            finished = Some(d);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    let finished = finished.expect("download finished");
    assert_eq!(std::fs::read(&finished.paths[0]).expect("file"), data);
    assert_eq!(sha(&std::fs::read(&finished.paths[0]).expect("file")), sha(&data));

    // The library lists it as unused until a model refers to it.
    assert!(app.library()[0].used_by.is_empty());
    let model_id = app.add_downloaded_model(&finished).expect("model created");
    assert_eq!(model_id, "Model-GGUF-Q4_K_M");
    assert!(app.config().models.iter().any(|m| m.id == model_id && m.files.model == finished.paths[0]));
    assert_eq!(app.library()[0].used_by, std::slice::from_ref(&model_id));
    assert!(app.delete_library_file(std::path::Path::new(&finished.paths[0])).is_err(), "a file in use is not deleted");

    // It persisted to config.json.
    let on_disk = std::fs::read_to_string(app.paths().config_dir.join("config.json")).expect("config.json");
    assert!(on_disk.contains("Model-GGUF-Q4_K_M") && on_disk.contains("deadbeefcafe"));

    // Update check: same commit as the download, so it is current.
    let checks = app.check_model_updates(None).await;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].verdict, crate::hf::update::Verdict::UpToDate, "{}", checks[0].message);
    assert_eq!(checks[0].latest_sha.as_deref(), Some("deadbeefcafe"));

    // A model downloaded at an older commit, whose file is now a different size, is flagged.
    let mut cfg = (*app.config()).clone();
    if let Some(crate::config::schema::ModelSource::Huggingface { commit_sha, .. }) = &mut cfg.models[0].source {
        *commit_sha = Some("0ldc0mm1t".into());
    }
    std::fs::write(&finished.paths[0], b"short").expect("shrink the local copy");
    app.save_config(cfg).expect("save");
    let stale = app.check_model_updates(None).await;
    assert_eq!(stale[0].verdict, crate::hf::update::Verdict::FilesChanged, "{}", stale[0].message);
    app.shutdown().await;
}

/// Test load starts the model, reports, and unloads it again; the diagnostics
/// zip carries the config without secrets or personal paths.
#[tokio::test]
async fn test_load_reports_and_unloads_and_diagnostics_are_redacted() {
    let model = std::env::temp_dir().join("tl-model.gguf");
    std::fs::write(&model, b"gguf").expect("model file");
    let cfg = json!({
        "server": { "port": 0 },
        "models": [{ "id": "m", "files": { "model": model.to_string_lossy() } }],
    });
    let root = tempfile::tempdir().expect("tempdir");
    let config_dir = root.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(config_dir.join("config.json"), cfg.to_string()).expect("write config");
    let paths = AppPaths { config_dir, data_dir: root.path().join("data"), bundled_engine_dir: None };
    let probe = Arc::new(crate::gate::test_support::FakeProbe { ready: std::sync::atomic::AtomicBool::new(true) });
    let services = Services {
        launcher: Some(Arc::new(FakeLauncher::default())),
        probe: Some(probe),
        secrets: Some(Arc::new(MemorySecretStore::default())),
        ..Services::default()
    };
    let app = RunningApp::start(paths, services).await.expect("app starts");

    let report = app.test_load("m").await;
    assert!(report.ok, "{}", report.message);
    assert!(report.load_seconds >= 0.0);
    let after = app.controller.status().await.expect("status");
    assert_eq!(after.models[0].state, ModelStateKind::Unloaded, "a test load leaves the model unloaded");

    let missing = app.test_load("nope").await;
    assert!(!missing.ok);
    assert!(!missing.message.is_empty());

    let zip_path = root.path().join("diag.zip");
    app.export_diagnostics(&zip_path).await.expect("bundle");
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).expect("open")).expect("zip");
    let names: Vec<String> = archive.file_names().filter_map(Result::ok).map(|n| n.into_owned()).collect();
    assert!(names.contains(&"config.json".to_owned()) && names.contains(&"system.txt".to_owned()), "{names:?}");
    let mut config_text = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("config.json").expect("config"), &mut config_text)
        .expect("read");
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    if !home.is_empty() {
        let escaped = home.replace('\\', "\\\\").to_ascii_lowercase();
        assert!(!config_text.to_ascii_lowercase().contains(&escaped));
    }
    app.shutdown().await;
}

/// A model that cannot start is reported with the reason, not as a success.
#[tokio::test]
async fn test_load_of_a_model_that_fails_to_start_says_why() {
    let model = std::env::temp_dir().join("tl-fail.gguf");
    std::fs::write(&model, b"gguf").expect("model file");
    let cfg = json!({
        "server": { "port": 0 },
        "models": [{ "id": "m", "files": { "model": model.to_string_lossy() } }],
    });
    let root = tempfile::tempdir().expect("tempdir");
    let config_dir = root.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("mkdir");
    std::fs::write(config_dir.join("config.json"), cfg.to_string()).expect("write config");
    let paths = AppPaths { config_dir, data_dir: root.path().join("data"), bundled_engine_dir: None };
    let launcher = Arc::new(FakeLauncher::default());
    launcher.fail_next("the engine exploded");
    let services = Services {
        launcher: Some(launcher),
        secrets: Some(Arc::new(MemorySecretStore::default())),
        ..Services::default()
    };
    let app = RunningApp::start(paths, services).await.expect("app starts");
    let report = app.test_load("m").await;
    assert!(!report.ok);
    assert!(report.message.contains("exploded"), "{}", report.message);
    app.shutdown().await;
}

/// Findings that are not errors never block a save; an error does (spec §14.4).
#[tokio::test]
async fn save_config_ignores_warnings_but_refuses_a_missing_required_draft() {
    use crate::config::schema::ModelConfig;
    use crate::config::validate::Severity;

    let (root, app) = start(json!({ "server": { "port": 0 } }), Arc::new(MemorySecretStore::default())).await;
    let main = root.path().join("main.gguf");
    std::fs::write(&main, crate::gguf::header::tests::sample_gguf("qwen3", 8, 4096, 64, 4, 4, 15)).expect("write");
    let mut model = ModelConfig { id: "m".into(), ..ModelConfig::default() };
    model.files.model = main.to_string_lossy().into_owned();
    // A reserved key in llama_args is only a warning.
    model.llama_args.insert("port".into(), json!(1234));
    let mut cfg = (*app.config()).clone();
    cfg.models = vec![model.clone()];
    app.save_config(cfg.clone()).expect("a warning does not block saving");
    let shown = app.validate_config(&cfg);
    assert!(shown.iter().any(|i| i.severity == Severity::Warning), "the warning is still reported: {shown:?}");

    // A draft type with no draft file cannot work: that is an error.
    model.llama_args.insert("spec-type".into(), json!("draft-simple"));
    cfg.models = vec![model];
    let errors = app.save_config(cfg).expect_err("a required draft file is missing");
    assert!(errors.iter().all(|i| i.severity == Severity::Error), "only errors are returned: {errors:?}");
    assert!(errors.iter().any(|i| i.path == "models[0].llama_args.spec-type"));
    app.shutdown().await;
}
