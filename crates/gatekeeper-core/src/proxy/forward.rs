//! Forwarding admitted requests to llama-server (spec §8.4).
//!
//! Responsibility: rewrite headers, send the request to the child, and
//! stream the response back wrapped so cancellation and in-flight counting
//! work.
//! Not responsible for: admission (see `gate::controller`) or error
//! formatting (see `errors`).
//!
//! Implements FR-REQ-5, FR-GAME-3.

use axum::body::Body;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use serde_json::json;
use tracing::{debug, warn};

use super::cancel_body::{CancelStyle, CancelableBody};
use super::errors::{ApiFamily, cancelled_rejection, rejection_response};
use super::failure_log::{FailureContext, FailureTap};
use crate::config::schema::Config;
use crate::gate::controller::Admitted;
use crate::gate::rejection::{RejectKind, Rejection};

/// Shared HTTP client for talking to children; pools one connection set per
/// child port.
pub type UpstreamClient = Client<HttpConnector, Body>;

/// Builds the shared upstream client.
pub fn new_client() -> UpstreamClient {
    Client::builder(hyper_util::rt::TokioExecutor::new()).build_http()
}

/// Headers that apply to a single connection and must not be forwarded
/// (RFC 9110 §7.6.1).
fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Copies request headers for the child. Client credentials and `Host` are
/// dropped; the child's own key is added (spec §8.4).
fn upstream_headers(src: &HeaderMap, api_key: &str) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in src {
        let n = name.as_str();
        if is_hop_by_hop(name) || matches!(n, "host" | "authorization" | "x-api-key" | "content-length") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    if let Ok(v) = HeaderValue::from_str(&format!("Bearer {api_key}")) {
        out.insert(header::AUTHORIZATION, v);
    }
    out
}

/// What the caller knows about the request being forwarded.
pub struct ForwardRequest<'a> {
    /// HTTP method.
    pub method: Method,
    /// Path plus query to request from the child.
    pub path_and_query: &'a str,
    /// Client headers.
    pub headers: &'a HeaderMap,
    /// Request body, unmodified.
    pub body: Bytes,
    /// Error format family.
    pub family: ApiFamily,
}

/// Sends `req` to the admitted child and returns the client response.
pub async fn forward(
    client: &UpstreamClient,
    cfg: &Config,
    admitted: Admitted,
    req: ForwardRequest<'_>,
) -> Response<Body> {
    let Admitted { model, upstream, guard, cancel } = admitted;
    let endpoint = req.path_and_query.split('?').next().unwrap_or_default().to_owned();
    let uri = format!("http://127.0.0.1:{}{}", upstream.port, req.path_and_query);

    let mut builder = Request::builder().method(req.method).uri(uri);
    if let Some(h) = builder.headers_mut() {
        *h = upstream_headers(req.headers, &upstream.api_key);
    }
    let Ok(upstream_req) = builder.body(Body::from(req.body)) else {
        return rejection_response(
            req.family,
            &Rejection::new(RejectKind::ModelCrashed, &model, "Could not build the upstream request."),
            &cfg.errors,
        );
    };

    // Waiting for the upstream's response headers is where non-streaming
    // requests spend their time, so cancellation must be able to interrupt it.
    let upstream_resp = tokio::select! {
        resp = client.request(upstream_req) => resp,
        () = cancel.cancelled() => {
            let info = cancel.reason();
            debug!(model = %model, cause = info.cause.as_str(), "request cancelled before response");
            return rejection_response(req.family, &cancelled_rejection(&info, &model), &cfg.errors);
        }
    };

    let upstream_resp = match upstream_resp {
        Ok(r) => r,
        Err(e) => {
            // The controller learns of a dead child through its exit watcher;
            // the client just needs a clear error (spec §8.4).
            warn!(model = %model, error = %e, "upstream request failed");
            let rejection =
                Rejection::new(RejectKind::ModelCrashed, &model, format!("Model '{model}' did not respond: {e}"))
                    .with_detail(json!({ "error": e.to_string() }));
            return rejection_response(req.family, &rejection, &cfg.errors);
        }
    };

    let (mut parts, incoming) = upstream_resp.into_parts();
    let is_event_stream = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/event-stream"));
    let hop: Vec<HeaderName> = parts.headers.keys().filter(|k| is_hop_by_hop(k)).cloned().collect();
    for name in hop {
        parts.headers.remove(name);
    }
    // Normalise the status in case the child sent something unusual.
    if parts.status == StatusCode::SWITCHING_PROTOCOLS {
        parts.status = StatusCode::BAD_GATEWAY;
    }

    let failed = parts.status.as_u16() >= 400;
    let context = FailureContext { model: model.clone(), endpoint, status: parts.status.as_u16() };
    let style = CancelStyle { family: req.family, is_event_stream, model, errors: cfg.errors.clone() };
    let body = CancelableBody::new(incoming, cancel, guard, style);
    // An error answer from llama-server is logged as it passes by (spec §20).
    let body = if failed { Body::new(FailureTap::new(body, context)) } else { Body::new(body) };
    Response::from_parts(parts, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_credentials_and_hop_by_hop_headers_are_replaced() {
        let mut src = HeaderMap::new();
        src.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer client-secret"));
        src.insert("x-api-key", HeaderValue::from_static("client-secret"));
        src.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
        src.insert(header::HOST, HeaderValue::from_static("example"));
        src.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let out = upstream_headers(&src, "child-key");
        assert_eq!(out[header::AUTHORIZATION], "Bearer child-key");
        assert!(out.get("x-api-key").is_none());
        assert!(out.get(header::CONNECTION).is_none());
        assert!(out.get(header::HOST).is_none());
        assert_eq!(out[header::CONTENT_TYPE], "application/json");
    }
}
