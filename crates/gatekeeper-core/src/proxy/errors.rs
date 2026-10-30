//! Error bodies in the client library's own format (spec §8.5).
//!
//! Responsibility: turn a gate [`Rejection`] or a cancelled stream into the
//! OpenAI-style or Anthropic-style JSON that client libraries surface
//! properly, with the right status code and `Retry-After`.
//! Not responsible for: deciding to reject (see `gate::controller`).
//!
//! Implements FR-REQ-4, FR-REQ-5.

use axum::body::Body;
use axum::http::{HeaderValue, Response, StatusCode, header};
use bytes::Bytes;
use serde_json::{Value, json};

use super::inflight::CancelInfo;
use crate::config::schema::ErrorsConfig;
use crate::gate::rejection::{RejectKind, Rejection};

/// Which client library family a request belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiFamily {
    /// OpenAI-compatible endpoints.
    OpenAi,
    /// Anthropic-style `/v1/messages`.
    Anthropic,
}

/// HTTP status for a refusal (spec §8.5 reason-code table).
pub fn status_for(kind: RejectKind, cfg: &ErrorsConfig) -> StatusCode {
    let gated = StatusCode::from_u16(cfg.status_code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
    match kind {
        RejectKind::GameRunning
        | RejectKind::Paused
        | RejectKind::VramGuard
        | RejectKind::RequestCancelled
        | RejectKind::ModelLoadFailed => gated,
        RejectKind::LoadTimeout => StatusCode::GATEWAY_TIMEOUT,
        RejectKind::ModelCrashed => StatusCode::BAD_GATEWAY,
        RejectKind::UnknownModel => StatusCode::NOT_FOUND,
        RejectKind::ShuttingDown => StatusCode::SERVICE_UNAVAILABLE,
    }
}

/// The `type` field: a coarse class clients may switch on.
fn type_for(kind: RejectKind) -> &'static str {
    match kind {
        RejectKind::GameRunning | RejectKind::Paused | RejectKind::VramGuard | RejectKind::ModelLoadFailed => {
            "model_not_loaded"
        }
        RejectKind::RequestCancelled => "request_cancelled",
        RejectKind::UnknownModel => "invalid_request_error",
        RejectKind::LoadTimeout | RejectKind::ModelCrashed | RejectKind::ShuttingDown => "server_error",
    }
}

/// Builds the JSON object placed under `error` (OpenAI) for a rejection.
pub fn openai_error_object(r: &Rejection, cfg: &ErrorsConfig) -> Value {
    let mut obj = json!({
        "message": r.message,
        "type": type_for(r.kind),
        "code": r.kind.code(),
        "param": Value::Null,
    });
    if cfg.include_details {
        obj["gatekeeper"] = json!({
            "model": r.model,
            "reason": r.kind.code(),
            "detail": r.detail,
            "retry_after_seconds": r.retry_after_seconds,
        });
    }
    obj
}

/// Builds the full response for a rejection.
pub fn rejection_response(family: ApiFamily, r: &Rejection, cfg: &ErrorsConfig) -> Response<Body> {
    let body = match family {
        ApiFamily::OpenAi => json!({ "error": openai_error_object(r, cfg) }),
        ApiFamily::Anthropic => json!({
            "type": "error",
            "error": { "type": type_for(r.kind), "message": r.message },
        }),
    };
    json_response(status_for(r.kind, cfg), &body, r.retry_after_seconds)
}

/// Builds a plain error for conditions outside the gate (bad request, 401,
/// 413, 404 for unknown paths).
pub fn simple_error(family: ApiFamily, status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = match family {
        ApiFamily::OpenAi => json!({
            "error": { "message": message, "type": "invalid_request_error", "code": code, "param": Value::Null }
        }),
        ApiFamily::Anthropic => json!({
            "type": "error",
            "error": { "type": "invalid_request_error", "message": message },
        }),
    };
    json_response(status, &body, None)
}

/// Serialises `body` with the given status and optional `Retry-After`.
pub fn json_response(status: StatusCode, body: &Value, retry_after: Option<u32>) -> Response<Body> {
    let mut resp = Response::new(Body::from(body.to_string()));
    *resp.status_mut() = status;
    resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Some(secs) = retry_after
        && let Ok(v) = HeaderValue::from_str(&secs.to_string())
    {
        resp.headers_mut().insert(header::RETRY_AFTER, v);
    }
    resp
}

/// The final SSE chunk sent when a stream that already started must be
/// cancelled (spec §8.5). No `[DONE]` follows, so clients know the stream
/// did not complete normally.
pub fn stream_error_chunk(family: ApiFamily, info: &CancelInfo, model: &str, cfg: &ErrorsConfig) -> Bytes {
    let rejection = cancelled_rejection(info, model);
    let text = match family {
        ApiFamily::OpenAi => format!("data: {}\n\n", json!({ "error": openai_error_object(&rejection, cfg) })),
        ApiFamily::Anthropic => format!(
            "event: error\ndata: {}\n\n",
            json!({ "type": "error", "error": { "type": "request_cancelled", "message": info.message } })
        ),
    };
    Bytes::from(text)
}

/// Rejection for a request cancelled before any response byte was sent.
pub fn cancelled_rejection(info: &CancelInfo, model: &str) -> Rejection {
    Rejection::new(RejectKind::RequestCancelled, model, info.message.clone())
        .with_detail(json!({ "cause": info.cause.as_str() }))
}

#[cfg(test)]
mod tests {
    use super::super::inflight::CancelCause;
    use super::*;
    use crate::gate::model_state::UnloadCause;

    fn cfg() -> ErrorsConfig {
        ErrorsConfig::default()
    }

    fn game_rejection() -> Rejection {
        Rejection::new(
            RejectKind::GameRunning,
            "qwen3-8b",
            "Model 'qwen3-8b' is not loaded because a game is running (eldenring.exe).",
        )
        .with_detail(json!({ "game": "eldenring.exe", "source": "process_list" }))
        .with_retry_after(30)
    }

    async fn body_json(resp: Response<Body>) -> Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn openai_game_error_matches_spec_example() {
        let resp = rejection_response(ApiFamily::OpenAi, &game_rejection(), &cfg());
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(resp.headers()[header::RETRY_AFTER], "30");
        let v = body_json(resp).await;
        assert_eq!(v["error"]["type"], "model_not_loaded");
        assert_eq!(v["error"]["code"], "game_running");
        assert_eq!(v["error"]["gatekeeper"]["detail"]["game"], "eldenring.exe");
        assert_eq!(v["error"]["gatekeeper"]["retry_after_seconds"], 30);
    }

    #[tokio::test]
    async fn anthropic_error_has_anthropic_shape() {
        let v = body_json(rejection_response(ApiFamily::Anthropic, &game_rejection(), &cfg())).await;
        assert_eq!(v["type"], "error");
        assert_eq!(v["error"]["type"], "model_not_loaded");
        assert!(v["error"]["message"].as_str().expect("msg").contains("eldenring.exe"));
    }

    #[tokio::test]
    async fn details_can_be_disabled() {
        let c = ErrorsConfig { include_details: false, ..cfg() };
        let v = body_json(rejection_response(ApiFamily::OpenAi, &game_rejection(), &c)).await;
        assert!(v["error"].get("gatekeeper").is_none());
    }

    #[test]
    fn status_codes_follow_the_reason_table_and_config() {
        let c = ErrorsConfig { status_code: 423, ..cfg() };
        assert_eq!(status_for(RejectKind::GameRunning, &c), StatusCode::LOCKED);
        assert_eq!(status_for(RejectKind::LoadTimeout, &c), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(status_for(RejectKind::ModelCrashed, &c), StatusCode::BAD_GATEWAY);
        assert_eq!(status_for(RejectKind::UnknownModel, &c), StatusCode::NOT_FOUND);
    }

    #[test]
    fn stream_error_chunks_have_no_done_marker() {
        let info = CancelInfo {
            cause: CancelCause::Unload(UnloadCause::Game),
            message: "unloaded because a game started".into(),
        };
        let openai = stream_error_chunk(ApiFamily::OpenAi, &info, "m", &cfg());
        let text = std::str::from_utf8(&openai).expect("utf8");
        assert!(text.starts_with("data: {"));
        assert!(text.ends_with("\n\n"));
        assert!(!text.contains("[DONE]"));
        let anth = stream_error_chunk(ApiFamily::Anthropic, &info, "m", &cfg());
        assert!(std::str::from_utf8(&anth).expect("utf8").starts_with("event: error\ndata: {"));
    }
}
