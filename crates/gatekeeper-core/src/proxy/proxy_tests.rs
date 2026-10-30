//! End-to-end proxy tests: real HTTP in and out, a stub standing in for
//! llama-server, and the real gate controller in between (spec §22.2).
//!
//! These use real sockets, so they run on the real clock with short timers
//! instead of a paused one (a paused clock would jump forward whenever the
//! runtime waits on network I/O).

use std::convert::Infallible;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::server::{ProxyState, bind, serve};
use crate::config::schema::{Config, DrainPolicy};
use crate::detect::GameSignal;
use crate::gate::test_support::{Harness, config, harness};

// ----------------------------------------------------------------- stub

/// Counters the stub exposes to tests.
#[derive(Default)]
struct StubStats {
    /// Streams that were dropped before finishing (client or proxy hung up).
    dropped_streams: AtomicUsize,
    /// Streams that ran to completion.
    completed_streams: AtomicUsize,
}

/// Increments a counter when dropped unless disarmed.
struct DropCounter {
    stats: Arc<StubStats>,
    finished: bool,
}

impl Drop for DropCounter {
    fn drop(&mut self) {
        if self.finished {
            self.stats.completed_streams.fetch_add(1, Ordering::SeqCst);
        } else {
            self.stats.dropped_streams.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Chunks the stub streams per response, 50 ms apart (about 2.5 s total).
const STUB_CHUNKS: usize = 50;

/// Stub `POST /v1/chat/completions` (also used for `/v1/messages`).
async fn stub_chat(State(stats): State<Arc<StubStats>>, headers: HeaderMap, body: Bytes) -> Response {
    let req: Value = serde_json::from_slice(&body).unwrap_or_default();
    let seen_auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    // Error answers like llama-server's, for the failure-logging tests.
    match req["fail"].as_str() {
        Some("json") => {
            let body = json!({ "error": { "code": 500, "message": "Value is not xhigh, medium or low", "type": "server_error" } });
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response();
        }
        Some("huge") => return (StatusCode::INTERNAL_SERVER_ERROR, "é".repeat(100_000)).into_response(),
        Some("binary") => return (StatusCode::INTERNAL_SERVER_ERROR, vec![0xffu8, 0xfe, 0x41]).into_response(),
        _ => {}
    }
    if req["stream"].as_bool().unwrap_or(false) {
        let guard = DropCounter { stats, finished: false };
        let stream = futures_util::stream::unfold((0usize, guard), |(i, mut guard)| async move {
            if i > STUB_CHUNKS {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            let chunk = if i == STUB_CHUNKS {
                guard.finished = true;
                Bytes::from_static(b"data: [DONE]\n\n")
            } else {
                Bytes::from(format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"))
            };
            Some((Ok::<_, Infallible>(chunk), (i + 1, guard)))
        });
        return Response::builder()
            .header("content-type", "text/event-stream")
            .header("x-seen-auth", seen_auth)
            .body(Body::from_stream(stream))
            .expect("response");
    }
    ([("x-seen-auth", seen_auth)], Json(json!({ "choices": [{ "message": { "content": "hello" } }], "echo": req })))
        .into_response()
}

/// Starts the stub on a random port.
async fn start_stub() -> (u16, Arc<StubStats>) {
    let stats = Arc::new(StubStats::default());
    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/props", get(|| async { Json(json!({ "ok": true })) }))
        .route("/v1/chat/completions", post(stub_chat))
        .route("/v1/messages", post(stub_chat))
        .with_state(stats.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (port, stats)
}

// -------------------------------------------------------------- harness

/// A running proxy in front of the real controller and a stub upstream.
struct Rig {
    /// Controller harness (fake launcher pointing at the stub).
    h: Harness,
    /// Stub counters.
    stats: Arc<StubStats>,
    /// `http://127.0.0.1:<port>`
    base: String,
    /// HTTP client.
    http: reqwest::Client,
    /// Shared API key slot.
    api_key: Arc<RwLock<Option<String>>>,
    /// Stops the proxy when the rig is dropped.
    shutdown: CancellationToken,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Test configuration: real-time friendly timers, port 0.
fn rig_config(ids: &[&str]) -> Config {
    let mut c = config(ids);
    c.server.port = 0;
    c.games.cooldown_seconds = 1;
    c.draining.timeout_seconds = 1;
    c.advanced.health_poll_interval_ms = 10;
    c
}

async fn rig(cfg: Config) -> Rig {
    let (stub_port, stats) = start_stub().await;
    let h = harness(cfg.clone());
    h.launcher.set_port(stub_port);

    let listener = bind(&cfg).await.expect("bind proxy");
    let port = listener.local_addr().expect("addr").port();
    let (_tx, rx) = watch::channel(Arc::new(cfg));
    // Keep the sender alive for the test's duration.
    std::mem::forget(_tx);
    let api_key = Arc::new(RwLock::new(None));
    let shutdown = CancellationToken::new();
    let state = ProxyState::new(h.handle.clone(), rx, api_key.clone());
    tokio::spawn(serve(listener, state, shutdown.clone()));
    Rig { h, stats, base: format!("http://127.0.0.1:{port}"), http: reqwest::Client::new(), api_key, shutdown }
}

impl Rig {
    fn chat(&self, model: &str, stream: bool) -> reqwest::RequestBuilder {
        self.http.post(format!("{}/v1/chat/completions", self.base)).json(&json!({
            "model": model, "stream": stream, "messages": [{ "role": "user", "content": "hi" }]
        }))
    }

    /// Waits until `cond` holds or panics after 5 seconds.
    async fn wait_for<F: Fn() -> bool>(&self, what: &str, cond: F) {
        for _ in 0..100 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }
}

/// Reads a streaming response fully into text.
async fn read_stream(resp: reqwest::Response) -> String {
    let mut out = String::new();
    let mut s = resp.bytes_stream();
    while let Some(chunk) = s.next().await {
        match chunk {
            Ok(b) => out.push_str(&String::from_utf8_lossy(&b)),
            Err(_) => break,
        }
    }
    out
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn forwards_json_and_replaces_client_credentials_with_the_childs_key() {
    let r = rig(rig_config(&["a"])).await;
    let resp = r.chat("a", false).header("authorization", "Bearer client-secret").send().await.expect("send");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["x-seen-auth"], "Bearer k", "child sees its own key, never the client's");
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["choices"][0]["message"]["content"], "hello");
    assert_eq!(v["echo"]["model"], "a", "body forwarded unmodified");
}

#[tokio::test]
async fn unknown_model_gets_404_in_openai_format() {
    let r = rig(rig_config(&["a"])).await;
    let resp = r.chat("nope", false).send().await.expect("send");
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["code"], "unknown_model");
}

#[tokio::test]
async fn missing_model_uses_default_model_or_is_a_400() {
    let mut cfg = rig_config(&["a"]);
    let r = rig(cfg.clone()).await;
    let resp = r
        .http
        .post(format!("{}/v1/chat/completions", r.base))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 400);

    cfg.server.default_model = Some("a".into());
    let r = rig(cfg).await;
    let resp = r
        .http
        .post(format!("{}/v1/chat/completions", r.base))
        .json(&json!({ "messages": [] }))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn game_running_returns_503_with_structured_error_and_retry_after() {
    let r = rig(rig_config(&["a"])).await;
    r.h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));
    r.h.settle().await;
    let resp = r.chat("a", false).send().await.expect("send");
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.headers()["retry-after"], "30");
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["type"], "model_not_loaded");
    assert_eq!(v["error"]["code"], "game_running");
    assert_eq!(v["error"]["gatekeeper"]["detail"]["game"], "eldenring.exe");
}

#[tokio::test]
async fn anthropic_endpoint_uses_anthropic_error_shape() {
    let r = rig(rig_config(&["a"])).await;
    r.h.handle.game_signal(GameSignal::active("process_list", "x.exe"));
    r.h.settle().await;
    let resp = r
        .http
        .post(format!("{}/v1/messages", r.base))
        .json(&json!({ "model": "a", "messages": [] }))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 503);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["type"], "error");
    assert_eq!(v["error"]["type"], "model_not_loaded");
}

#[tokio::test]
async fn models_endpoint_reports_state_and_block_reason() {
    let r = rig(rig_config(&["a", "b"])).await;
    let v: Value = r.http.get(format!("{}/v1/models", r.base)).send().await.expect("send").json().await.expect("json");
    assert_eq!(v["data"].as_array().expect("array").len(), 2);
    assert_eq!(v["data"][0]["gatekeeper"]["state"], "unloaded");
    assert_eq!(v["data"][0]["gatekeeper"]["blocked_reason"], Value::Null);

    r.h.handle.game_signal(GameSignal::active("process_list", "x.exe"));
    r.h.settle().await;
    let v: Value = r.http.get(format!("{}/v1/models", r.base)).send().await.expect("send").json().await.expect("json");
    assert_eq!(v["data"][0]["gatekeeper"]["blocked_reason"], "game_running");
}

#[tokio::test]
async fn cancel_policy_ends_a_running_stream_with_an_error_event_and_no_done() {
    let mut cfg = rig_config(&["a"]);
    cfg.draining.policy = DrainPolicy::Cancel;
    let r = rig(cfg).await;

    let resp = r.chat("a", true).send().await.expect("send");
    assert_eq!(resp.status(), 200);
    let reader = tokio::spawn(read_stream(resp));
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));

    let text = reader.await.expect("join");
    assert!(text.contains("\"t0\""), "some tokens arrived before the cancel: {text}");
    assert!(text.contains("\"error\""), "error delivered in-stream: {text}");
    assert!(text.contains("request_cancelled"));
    assert!(text.contains("eldenring.exe"));
    assert!(!text.contains("[DONE]"), "no [DONE]: the stream did not complete normally");
    r.wait_for("model unloaded", || !r.h.launcher.running("a")).await;
}

#[tokio::test]
async fn finish_policy_lets_the_stream_complete_then_unloads() {
    let mut cfg = rig_config(&["a"]);
    cfg.draining.policy = DrainPolicy::Finish;
    let r = rig(cfg).await;

    let resp = r.chat("a", true).send().await.expect("send");
    let reader = tokio::spawn(read_stream(resp));
    tokio::time::sleep(Duration::from_millis(200)).await;
    r.h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));

    let text = reader.await.expect("join");
    assert!(text.contains("[DONE]"), "stream finished normally");
    assert!(!text.contains("request_cancelled"));
    r.wait_for("model unloaded after drain", || !r.h.launcher.running("a")).await;
    // And new requests are now refused.
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 503);
}

#[tokio::test]
async fn wait_then_cancel_cancels_after_the_timeout() {
    let mut cfg = rig_config(&["a"]);
    cfg.draining.policy = DrainPolicy::WaitThenCancel;
    cfg.draining.timeout_seconds = 1;
    let r = rig(cfg).await;
    let resp = r.chat("a", true).send().await.expect("send");
    let reader = tokio::spawn(read_stream(resp));
    tokio::time::sleep(Duration::from_millis(200)).await;
    r.h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));
    let text = reader.await.expect("join");
    assert!(text.contains("request_cancelled") && !text.contains("[DONE]"), "{text}");
}

#[tokio::test]
async fn client_disconnect_stops_the_upstream_stream_and_releases_the_inflight_count() {
    let r = rig(rig_config(&["a"])).await;
    let resp = r.chat("a", true).send().await.expect("send");
    let mut s = resp.bytes_stream();
    let first = s.next().await.expect("first chunk").expect("ok");
    assert!(!first.is_empty());
    assert_eq!(r.h.status().await.models[0].inflight, 1);

    drop(s); // client hangs up mid-stream (spec §21)
    r.wait_for("upstream stream dropped", || r.stats.dropped_streams.load(Ordering::SeqCst) == 1).await;
    for _ in 0..40 {
        if r.h.status().await.models[0].inflight == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("in-flight count never returned to zero");
}

#[tokio::test]
async fn passthrough_reaches_native_endpoints() {
    let r = rig(rig_config(&["a"])).await;
    let v: Value = r.http.get(format!("{}/m/a/props", r.base)).send().await.expect("send").json().await.expect("json");
    assert_eq!(v["ok"], true);
}

#[tokio::test]
async fn foreign_host_header_is_refused() {
    let r = rig(rig_config(&["a"])).await;
    let resp = r.chat("a", false).header("host", "attacker.example").send().await.expect("send");
    assert_eq!(resp.status(), 403);
}

#[tokio::test]
async fn api_key_is_enforced_when_set_but_health_stays_open() {
    let r = rig(rig_config(&["a"])).await;
    *r.api_key.write().expect("lock") = Some("secret".into());
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 401);
    assert_eq!(r.chat("a", false).header("authorization", "Bearer wrong").send().await.expect("send").status(), 401);
    assert_eq!(r.chat("a", false).header("authorization", "Bearer secret").send().await.expect("send").status(), 200);
    assert_eq!(r.chat("a", false).header("x-api-key", "secret").send().await.expect("send").status(), 200);
    assert_eq!(r.http.get(format!("{}/health", r.base)).send().await.expect("send").status(), 200);
}

#[tokio::test]
async fn oversized_body_is_a_413() {
    let mut cfg = rig_config(&["a"]);
    cfg.server.max_request_body_mib = 1;
    let r = rig(cfg).await;
    let big = "x".repeat(2 * 1024 * 1024);
    let resp = r
        .http
        .post(format!("{}/v1/chat/completions", r.base))
        .json(&json!({ "model": "a", "pad": big }))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), 413);
    let v: Value = resp.json().await.expect("json");
    assert_eq!(v["error"]["code"], "request_too_large");
}

#[tokio::test]
async fn cors_is_off_by_default_and_opt_in_per_origin() {
    let mut cfg = rig_config(&["a"]);
    let r = rig(cfg.clone()).await;
    let resp =
        r.http.get(format!("{}/v1/models", r.base)).header("origin", "https://app.example").send().await.expect("send");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(resp.headers().get("access-control-allow-origin").is_none());

    cfg.server.cors_allowed_origins = vec!["https://app.example".into()];
    let r = rig(cfg).await;
    let resp =
        r.http.get(format!("{}/v1/models", r.base)).header("origin", "https://app.example").send().await.expect("send");
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()["access-control-allow-origin"], "https://app.example");
    let other = r
        .http
        .get(format!("{}/v1/models", r.base))
        .header("origin", "https://evil.example")
        .send()
        .await
        .expect("send");
    assert_eq!(other.status(), StatusCode::FORBIDDEN);
    assert!(other.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn browser_origin_rejection_cannot_mutate_admin_state() {
    let mut cfg = rig_config(&["a"]);
    cfg.server.cors_allowed_origins = vec!["http://tauri.localhost".into(), "tauri://localhost".into()];
    let r = rig(cfg).await;

    // A simple POST can reach a loopback listener without a CORS preflight.
    // Reject it before controller dispatch, not merely in response headers.
    for origin in [
        "https://attacker.example",
        "null",
        "http://tauri.localhost/path",
        "http://tauri.localhost https://attacker.example",
    ] {
        let response = r
            .http
            .post(format!("{}/gatekeeper/v1/pause", r.base))
            .header("origin", origin)
            .header("content-type", "text/plain")
            .body("")
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "origin {origin}");
        assert!(response.headers().get("access-control-allow-origin").is_none());
        let status = r.h.handle.status().await.expect("status");
        assert!(!status.paused, "rejected browser origin must not pause the gate");
    }

    // Preserve exact configured Tauri origins and native clients with no Origin.
    for origin in [Some("http://tauri.localhost"), Some("tauri://localhost"), None] {
        let mut request = r.http.post(format!("{}/gatekeeper/v1/pause", r.base));
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = request.send().await.expect("send");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(r.h.handle.status().await.expect("status").paused);
        let response = r.http.post(format!("{}/gatekeeper/v1/resume", r.base)).send().await.expect("send");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!r.h.handle.status().await.expect("status").paused);
    }
}

#[tokio::test]
async fn browser_origin_rejects_duplicate_headers_before_admin_dispatch() {
    let mut cfg = rig_config(&["a"]);
    cfg.server.cors_allowed_origins = vec!["https://app.example".into()];
    let r = rig(cfg).await;
    let mut origins = reqwest::header::HeaderMap::new();
    origins.append("origin", reqwest::header::HeaderValue::from_static("https://app.example"));
    origins.append("origin", reqwest::header::HeaderValue::from_static("https://attacker.example"));
    let response = r.http.post(format!("{}/gatekeeper/v1/pause", r.base)).headers(origins).send().await.expect("send");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(!r.h.handle.status().await.expect("status").paused);
}

#[tokio::test]
async fn admin_api_reports_status_and_pauses() {
    let r = rig(rig_config(&["a"])).await;
    let v: Value =
        r.http.get(format!("{}/gatekeeper/v1/status", r.base)).send().await.expect("send").json().await.expect("json");
    assert_eq!(v["paused"], false);

    assert_eq!(r.http.post(format!("{}/gatekeeper/v1/pause", r.base)).send().await.expect("send").status(), 200);
    r.h.settle().await;
    let resp = r.chat("a", false).send().await.expect("send");
    assert_eq!(resp.status(), 503);
    assert_eq!(resp.json::<Value>().await.expect("json")["error"]["code"], "paused");

    r.http.post(format!("{}/gatekeeper/v1/resume", r.base)).send().await.expect("send");
    r.h.settle().await;
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 200);
}

#[tokio::test]
async fn admin_api_can_be_disabled() {
    let mut cfg = rig_config(&["a"]);
    cfg.server.admin_api_enabled = false;
    let r = rig(cfg).await;
    assert_eq!(r.http.get(format!("{}/gatekeeper/v1/status", r.base)).send().await.expect("send").status(), 404);
}

// ------------------------------------------------------ failure logging

/// A log sink the tests can read back.
#[derive(Clone, Default)]
struct LogBuf(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for LogBuf {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("lock")).into_owned()
    }

    /// The lines that say a request failed.
    fn failures(&self) -> Vec<String> {
        self.text().lines().filter(|l| l.contains("request failed")).map(str::to_owned).collect()
    }
}

/// Routes this thread's log output into a buffer at `debug` level, so a
/// success logged at `debug` would show up too. The tests run on a single
/// thread, which is where the proxy tasks run.
fn capture_logs() -> (LogBuf, tracing::subscriber::DefaultGuard) {
    let buf = LogBuf::default();
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

async fn post_failing(r: &Rig, how: &str) -> reqwest::Response {
    r.http
        .post(format!("{}/v1/chat/completions", r.base))
        .json(&json!({ "model": "a", "fail": how, "messages": [] }))
        .send()
        .await
        .expect("send")
}

#[tokio::test]
async fn upstream_error_status_is_logged_and_body_reaches_the_client() {
    let (log, _guard) = capture_logs();
    let r = rig(rig_config(&["a"])).await;
    let resp = post_failing(&r, "json").await;
    assert_eq!(resp.status(), 500);
    let v: Value = resp.json().await.expect("the client still gets llama-server's JSON");
    assert_eq!(v["error"]["message"], "Value is not xhigh, medium or low");

    let lines = log.failures();
    assert_eq!(lines.len(), 1, "{}", log.text());
    let line = &lines[0];
    assert!(line.contains("WARN"), "{line}");
    for part in
        ["model=\"a\"", "endpoint=\"/v1/chat/completions\"", "status=500", "error=Value is not xhigh, medium or low"]
    {
        assert!(line.contains(part), "missing {part} in {line}");
    }
}

#[tokio::test]
async fn huge_or_non_utf8_error_bodies_do_not_break_forwarding() {
    let (log, _guard) = capture_logs();
    let r = rig(rig_config(&["a"])).await;

    let huge = post_failing(&r, "huge").await;
    assert_eq!(huge.status(), 500);
    let body = huge.bytes().await.expect("body");
    assert_eq!(body.len(), 200_000, "every byte of an oversized error body reaches the client");
    assert!(body.iter().all(|b| [0xc3u8, 0xa9].contains(b)));

    let binary = post_failing(&r, "binary").await;
    assert_eq!(binary.bytes().await.expect("body").as_ref(), [0xff, 0xfe, 0x41], "bytes arrive unchanged");

    let lines = log.failures();
    assert_eq!(lines.len(), 2, "{}", log.text());
    let error_chars = lines[0].split("error=").nth(1).expect("error field").trim_end().chars().count();
    assert_eq!(error_chars, 301, "300 characters and an ellipsis: {}", lines[0]);
}

#[tokio::test]
async fn rejections_are_logged_at_warn() {
    let (log, _guard) = capture_logs();

    // Unknown model, unknown endpoint, missing model name, wrong API key, closed gate.
    let r = rig(rig_config(&["a"])).await;
    assert_eq!(r.chat("nope", false).send().await.expect("send").status(), 404);
    assert_eq!(r.http.get(format!("{}/definitely/not/here", r.base)).send().await.expect("send").status(), 404);
    let no_model = r.http.post(format!("{}/v1/chat/completions", r.base)).json(&json!({ "messages": [] }));
    assert_eq!(no_model.send().await.expect("send").status(), 400);
    *r.api_key.write().expect("lock") = Some("secret".into());
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 401);
    *r.api_key.write().expect("lock") = None;
    r.h.handle.game_signal(GameSignal::active("process_list", "eldenring.exe"));
    r.h.settle().await;
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 503);

    // Too large.
    let mut cfg = rig_config(&["a"]);
    cfg.server.max_request_body_mib = 1;
    let big = rig(cfg).await;
    let body = json!({ "model": "a", "pad": "x".repeat(2 * 1024 * 1024) });
    let resp = big.http.post(format!("{}/v1/chat/completions", big.base)).json(&body).send().await.expect("send");
    assert_eq!(resp.status(), 413);

    let text = log.failures().join(
        "
",
    );
    for (status, why) in [
        (404, "endpoint=\"/v1/chat/completions\""),
        (404, "endpoint=\"/definitely/not/here\""),
        (400, "model=\"\""),
        (401, "Missing or invalid API key"),
        (503, "model=\"a\""),
        (413, "exceeds the 1 MiB limit"),
    ] {
        assert!(
            log.failures().iter().any(|l| l.contains(&format!("status={status}")) && l.contains(why)),
            "no warning with status {status} and {why}:
{text}"
        );
    }
    assert!(log.failures().iter().all(|l| l.contains("WARN")));
}

#[tokio::test]
async fn successful_requests_are_not_logged_as_failures() {
    let (log, _guard) = capture_logs();
    let r = rig(rig_config(&["a"])).await;
    assert_eq!(r.chat("a", false).send().await.expect("send").status(), 200);
    assert!(log.failures().is_empty(), "{}", log.text());
    assert!(
        log.text().contains("DEBUG") && log.text().contains("request"),
        "a success is logged at debug: {}",
        log.text()
    );
}
