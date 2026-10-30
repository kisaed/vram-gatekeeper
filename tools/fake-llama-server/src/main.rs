//! A stand-in for `llama-server` used by tests and development (spec §22.1).
//!
//! It mimics what the gatekeeper depends on: the `--version` / `--help`
//! output, `/health` going from 503 to 200 after a load delay, bearer-key
//! authentication, streamed and non-streamed chat completions, log lines
//! about GPU layer placement, and noticing when a client hangs up.
//!
//! Behaviour is tuned with environment variables, which tests set through a
//! model's `env` map:
//!
//! | Variable | Meaning | Default |
//! |---|---|---|
//! | `FAKE_LOAD_MS` | time before `/health` turns 200 | 300 |
//! | `FAKE_LAYERS` | layer placement to log, `X/Y` | `33/33` |
//! | `FAKE_TOKENS` | tokens per completion | 40 |
//! | `FAKE_TOKEN_MS` | delay per token | 30 |
//! | `FAKE_CRASH_AFTER_MS` | exit with code 3 this long after start | never |
//! | `FAKE_FAIL_START` | exit immediately with an error message | unset |
//! | `FAKE_BIND_FAIL_ONCE` | path of a marker file; the first launch reports a bind failure and creates it | unset |

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// The help text printed for `--help`, modelled on llama.cpp's.
const HELP: &str = include_str!("help.txt");

/// Server state shared by handlers.
struct Fake {
    /// True once the simulated load finished.
    ready: AtomicBool,
    /// Required bearer key, if any.
    api_key: Option<String>,
    /// Tokens per completion.
    tokens: u32,
    /// Delay per token.
    token_delay: Duration,
    /// Model file name, echoed by `/v1/models`.
    model: String,
}

/// Reads an integer environment variable.
fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

/// Returns the value following `flag` in the argument list.
fn arg_value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // llama.cpp prints its version on stderr; mimic that.
    if args.iter().any(|a| a == "--version") {
        eprintln!("version: 9999 (fake0000)\nbuilt with fake-llama-server for tests");
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h" || a == "--usage") {
        print!("{HELP}");
        return;
    }
    if let Ok(msg) = std::env::var("FAKE_FAIL_START") {
        eprintln!("error: {msg}");
        std::process::exit(1);
    }

    let host = arg_value(&args, "--host").unwrap_or_else(|| "127.0.0.1".to_owned());
    let port: u16 = arg_value(&args, "--port").and_then(|p| p.parse().ok()).unwrap_or(8080);
    let model = arg_value(&args, "--model").unwrap_or_default();
    let state = Arc::new(Fake {
        ready: AtomicBool::new(false),
        api_key: arg_value(&args, "--api-key"),
        tokens: u32::try_from(env_u64("FAKE_TOKENS", 40)).unwrap_or(40),
        token_delay: Duration::from_millis(env_u64("FAKE_TOKEN_MS", 30)),
        model: std::path::Path::new(&model)
            .file_name()
            .map_or_else(|| "fake".to_owned(), |n| n.to_string_lossy().into_owned()),
    });

    // Simulates another process grabbing the port: fails the first launch only.
    if let Ok(marker) = std::env::var("FAKE_BIND_FAIL_ONCE")
        && !std::path::Path::new(&marker).exists()
    {
        let _ = std::fs::write(&marker, "failed once");
        eprintln!("error: couldn't bind HTTP server socket, hostname: {host}, port: {port}: simulated collision");
        std::process::exit(1);
    }

    let listener = match TcpListener::bind((host.as_str(), port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: couldn't bind HTTP server socket, hostname: {host}, port: {port}: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("main: loading model {model}");
    let layers = std::env::var("FAKE_LAYERS").unwrap_or_else(|_| "33/33".to_owned());
    eprintln!("load_tensors: offloading output layer to GPU");
    eprintln!("load_tensors: offloaded {layers} layers to GPU");

    if let Some(ms) = std::env::var("FAKE_CRASH_AFTER_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            eprintln!("fatal: simulated crash");
            std::process::exit(3);
        });
    }

    let load = Duration::from_millis(env_u64("FAKE_LOAD_MS", 300));
    let loader = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(load).await;
        loader.ready.store(true, Ordering::SeqCst);
        eprintln!("main: model loaded");
        eprintln!("srv  update_slots: all slots are idle");
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/props", get(props))
        .route("/v1/models", get(models))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/messages", post(chat))
        .route("/completion", post(chat))
        .route("/tokenize", post(tokenize))
        .with_state(state);
    eprintln!("main: server is listening on http://{host}:{port}");
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("error: server failed: {e}");
        std::process::exit(1);
    }
}

/// Rejects requests without the bearer key.
fn check_auth(state: &Fake, headers: &HeaderMap) -> Result<(), Response> {
    let Some(key) = &state.api_key else {
        return Ok(());
    };
    let given =
        headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    if given == Some(key.as_str()) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": { "message": "Invalid API Key", "type": "authentication_error" } })),
        )
            .into_response())
    }
}

/// `GET /health`: 503 while loading, 200 when ready. Never needs the key.
async fn health(State(s): State<Arc<Fake>>) -> Response {
    if s.ready.load(Ordering::SeqCst) {
        Json(json!({ "status": "ok" })).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": { "code": 503, "message": "Loading model", "type": "unavailable_error" } })),
        )
            .into_response()
    }
}

/// `GET /props`.
async fn props(State(s): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&s, &headers) {
        return r;
    }
    Json(json!({ "model_path": s.model, "total_slots": 1 })).into_response()
}

/// `GET /v1/models`.
async fn models(State(s): State<Arc<Fake>>, headers: HeaderMap) -> Response {
    if let Err(r) = check_auth(&s, &headers) {
        return r;
    }
    Json(json!({ "object": "list", "data": [{ "id": s.model, "object": "model" }] })).into_response()
}

/// `POST /tokenize`.
async fn tokenize(State(s): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&s, &headers) {
        return r;
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or_default();
    let n = v["content"].as_str().map_or(0, |t| t.split_whitespace().count());
    Json(json!({ "tokens": (0..n).collect::<Vec<_>>() })).into_response()
}

/// Logs when a stream is dropped before it finished, like llama-server
/// releasing a slot after a client disconnect (spec F9).
struct SlotGuard {
    /// Set when the stream ran to completion.
    done: bool,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        if !self.done {
            eprintln!("srv  stop: cancel task, id_task = 0 (client disconnected)");
        }
    }
}

/// `POST /v1/chat/completions` (and friends): streams or returns JSON.
async fn chat(State(s): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    if let Err(r) = check_auth(&s, &headers) {
        return r;
    }
    if !s.ready.load(Ordering::SeqCst) {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "error": { "message": "Loading model" } })))
            .into_response();
    }
    let req: Value = serde_json::from_slice(&body).unwrap_or_default();
    let model = req["model"].as_str().unwrap_or("fake").to_owned();
    eprintln!("srv  params_from_: chat request received for model {model}");

    if req["stream"].as_bool().unwrap_or(false) {
        let total = s.tokens;
        let delay = s.token_delay;
        let stream = futures_util::stream::unfold((0u32, SlotGuard { done: false }), move |(i, mut guard)| {
            let model = model.clone();
            async move {
                if i > total {
                    return None;
                }
                tokio::time::sleep(delay).await;
                let chunk = if i == total {
                    guard.done = true;
                    Bytes::from_static(b"data: [DONE]\n\n")
                } else {
                    let payload = json!({
                        "model": model,
                        "choices": [{ "index": 0, "delta": { "content": format!("t{i} ") }, "finish_reason": Value::Null }]
                    });
                    Bytes::from(format!("data: {payload}\n\n"))
                };
                Some((Ok::<_, Infallible>(chunk), (i + 1, guard)))
            }
        });
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header(header::CACHE_CONTROL, "no-cache")
            .body(Body::from_stream(stream))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }

    tokio::time::sleep(s.token_delay * s.tokens).await;
    let text: String = (0..s.tokens).map(|i| format!("t{i} ")).collect();
    Json(json!({
        "model": model,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 3, "completion_tokens": s.tokens, "total_tokens": s.tokens + 3 }
    }))
    .into_response()
}
