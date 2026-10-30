//! The HTTP listener and request pipeline (spec §8, §19).
//!
//! Responsibility: accept client connections and run each request through
//! host check, authentication, CORS, body limit, routing, admission and
//! forwarding, in that order.
//! Not responsible for: lifecycle decisions (the controller) or process
//! management.
//!
//! Implements FR-REQ-1..5.

use std::io;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode, header};
use bytes::Bytes;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::admin_api;
use super::errors::{ApiFamily, json_response, rejection_response, status_for};
use super::failure_log::{log_failure, refuse};
use super::forward::{ForwardRequest, UpstreamClient, forward, new_client};
use super::routing::{Route, body_hints, classify};
use crate::config::schema::Config;
use crate::gate::controller::ControllerHandle;
use crate::gate::model_state::ModelStateKind;
use crate::gate::rejection::{RejectKind, Rejection};
use crate::sync::RwLockExt;

/// Hosts always accepted (spec §19, DNS-rebinding protection).
const LOCAL_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// Everything a request handler needs; cheap to clone.
#[derive(Clone)]
pub struct ProxyState {
    /// Front door to the gate controller.
    pub controller: ControllerHandle,
    /// Live configuration.
    pub config: watch::Receiver<Arc<Config>>,
    /// API key for LAN mode, loaded from Credential Manager. `None` disables
    /// authentication (only allowed for loopback binds).
    pub api_key: Arc<RwLock<Option<String>>>,
    /// Client used to reach children.
    pub client: UpstreamClient,
    /// True when the listener accepts connections from the network. Requests
    /// are then refused unless an API key is set, whatever `config.json` says.
    pub lan_bound: bool,
}

impl ProxyState {
    /// Builds the state.
    pub fn new(
        controller: ControllerHandle,
        config: watch::Receiver<Arc<Config>>,
        api_key: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self { controller, config, api_key, client: new_client(), lan_bound: false }
    }
}

/// Binds the listener described by `cfg` (spec §8.1): loopback by default,
/// all interfaces only in LAN mode.
///
/// # Errors
/// Returns the OS error, for example when the port is already in use.
pub async fn bind(cfg: &Config) -> io::Result<TcpListener> {
    let host = if cfg.server.allow_lan { "0.0.0.0" } else { cfg.server.host.as_str() };
    TcpListener::bind((host, cfg.server.port)).await
}

/// Serves requests until `shutdown` fires.
///
/// # Errors
/// Returns an I/O error if the server fails.
pub async fn serve(listener: TcpListener, state: ProxyState, shutdown: CancellationToken) -> io::Result<()> {
    if let Ok(addr) = listener.local_addr() {
        info!(%addr, "proxy listening");
    }
    let app = Router::new().fallback(handle).with_state(state);
    axum::serve(listener, app).with_graceful_shutdown(async move { shutdown.cancelled().await }).await
}

/// Strips the port from a `Host` header value.
fn host_without_port(host: &str) -> &str {
    if host.starts_with('[') {
        // IPv6 literal: "[::1]:8088"
        return host.find(']').map_or(host, |i| &host[..=i]);
    }
    host.rsplit_once(':').map_or(host, |(h, _)| h)
}

/// True if the `Host` header names this machine or a configured LAN name.
fn host_allowed(cfg: &Config, headers: &HeaderMap) -> bool {
    let Some(raw) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        // HTTP/1.0 clients may omit it; a browser (the rebinding threat)
        // never does.
        return true;
    };
    let host = host_without_port(raw);
    LOCAL_HOSTS.iter().any(|h| h.eq_ignore_ascii_case(host))
        || cfg.server.allowed_host_names.iter().any(|h| h.eq_ignore_ascii_case(host))
}

/// Checks the client's API key against the configured one in constant time.
fn authorized(key: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(expected) = key else { return true };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")));
    let x_api_key = headers.get("x-api-key").and_then(|v| v.to_str().ok());
    [bearer, x_api_key].into_iter().flatten().any(|given| bool::from(given.as_bytes().ct_eq(expected.as_bytes())))
}

/// Accepts an Origin-less native client, or one explicitly approved browser origin.
///
/// CORS response headers alone do not prevent a simple cross-origin POST from
/// changing local state. Reject unapproved origins before any request dispatch.
fn checked_origin(cfg: &Config, headers: &HeaderMap) -> Result<Option<HeaderValue>, ()> {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(origin) = values.next() else { return Ok(None) };
    if values.next().is_some() {
        return Err(());
    }
    let text = origin.to_str().map_err(|_| ())?;
    if !serialized_origin(text) || !cfg.server.cors_allowed_origins.iter().any(|o| o == "*" || o == text) {
        return Err(());
    }
    Ok(Some(origin.clone()))
}

/// A single serialized origin, including an explicitly allowed opaque `null`.
fn serialized_origin(text: &str) -> bool {
    if text == "null" {
        return true;
    }
    if text.bytes().any(|b| !(0x21..=0x7e).contains(&b)) {
        return false;
    }
    let Some((_, authority)) = text.split_once("://") else { return false };
    if authority.is_empty() || authority.contains(['/', '\\', '?', '#', '@', ',']) {
        return false;
    }
    // Parse for valid scheme, host, IPv6 and port syntax, but keep exact origin
    // matching rather than normalizing a configured application's identity.
    reqwest::Url::parse(text).is_ok_and(|url| url.host_str().is_some())
}

/// Adds CORS headers to a response.
fn with_cors(mut resp: Response<Body>, origin: Option<HeaderValue>) -> Response<Body> {
    if let Some(o) = origin {
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    resp
}

/// Entry point for every request.
async fn handle(State(st): State<ProxyState>, req: Request<Body>) -> Response<Body> {
    let cfg = st.config.borrow().clone();
    let endpoint = req.uri().path().to_owned();
    let origin = match checked_origin(&cfg, req.headers()) {
        Ok(origin) => origin,
        Err(()) => {
            return refuse(
                ApiFamily::OpenAi,
                StatusCode::FORBIDDEN,
                "origin_not_allowed",
                "The Origin header is malformed or is not allowed.",
                "",
                &endpoint,
            );
        }
    };

    if !host_allowed(&cfg, req.headers()) {
        return refuse(
            ApiFamily::OpenAi,
            StatusCode::FORBIDDEN,
            "host_not_allowed",
            "The Host header is not allowed.",
            "",
            &endpoint,
        );
    }

    // CORS preflight carries no credentials, so it is answered before auth.
    if req.method() == Method::OPTIONS {
        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::NO_CONTENT;
        if origin.is_some() {
            let h = resp.headers_mut();
            h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, OPTIONS"));
            h.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type, x-api-key, anthropic-version"),
            );
        }
        return with_cors(resp, origin);
    }

    let route = classify(req.method().as_str(), req.uri().path());
    let family = match &route {
        Route::Model { family } => *family,
        _ => ApiFamily::OpenAi,
    };

    // /health is intentionally open: it reveals nothing but liveness.
    if route != Route::Health {
        let key = st.api_key.read_ok().clone();
        // Defence in depth for LAN mode (spec §8.1, §19): a listener open to the
        // network must never serve requests without a key, even if the key
        // vanished after startup or the config file was edited by hand.
        if st.lan_bound && key.is_none() {
            return with_cors(
                refuse(
                    family,
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                    "LAN access is disabled until an API key is set.",
                    "",
                    &endpoint,
                ),
                origin,
            );
        }
        if !authorized(key.as_deref(), req.headers()) {
            return with_cors(
                refuse(family, StatusCode::UNAUTHORIZED, "unauthorized", "Missing or invalid API key.", "", &endpoint),
                origin,
            );
        }
    }

    let resp = dispatch(&st, &cfg, route, family, req).await;
    with_cors(resp, origin)
}

/// Reads the body up to the configured limit.
async fn read_body(
    cfg: &Config,
    req: Request<Body>,
    family: ApiFamily,
) -> Result<(http::request::Parts, Bytes), Response<Body>> {
    let limit = (cfg.server.max_request_body_mib as usize).saturating_mul(1024 * 1024);
    let endpoint = req.uri().path().to_owned();
    let (parts, body) = req.into_parts();
    let too_large = || {
        refuse(
            family,
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            &format!("Request body exceeds the {} MiB limit.", cfg.server.max_request_body_mib),
            "",
            &endpoint,
        )
    };
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > limit)
    {
        drain(body).await;
        return Err(too_large());
    }
    match axum::body::to_bytes(body, limit).await {
        Ok(bytes) => Ok((parts, bytes)),
        // The limit was hit mid-upload (chunked body, no Content-Length).
        Err(_) => Err(too_large()),
    }
}

/// Upper bound on how much of a rejected upload is read and discarded.
const DRAIN_CAP_BYTES: usize = 512 * 1024 * 1024;

/// Discards (up to a cap) the rest of a rejected request body. Answering
/// before the client finished sending makes Windows reset the connection, so
/// the client would see a network error instead of our 413.
async fn drain(body: Body) {
    use futures_util::StreamExt;
    let mut stream = body.into_data_stream();
    let mut seen = 0usize;
    while let Some(Ok(chunk)) = stream.next().await {
        seen += chunk.len();
        if seen >= DRAIN_CAP_BYTES {
            break;
        }
    }
}

/// Routes a request that passed the front-door checks.
async fn dispatch(
    st: &ProxyState,
    cfg: &Config,
    route: Route,
    family: ApiFamily,
    req: Request<Body>,
) -> Response<Body> {
    let endpoint = req.uri().path().to_owned();
    match route {
        Route::Health => health(st).await,
        Route::ModelList => model_list(st, cfg).await,
        Route::NotFound => refuse(family, StatusCode::NOT_FOUND, "not_found", "Unknown endpoint.", "", &endpoint),
        Route::Admin(sub) => {
            if !cfg.server.admin_api_enabled {
                return refuse(family, StatusCode::NOT_FOUND, "not_found", "Unknown endpoint.", "", &endpoint);
            }
            let method = req.method().clone();
            match read_body(cfg, req, family).await {
                Ok((_, body)) => admin_api::handle(&st.controller, &cfg.errors, &method, &sub, &body).await,
                Err(resp) => resp,
            }
        }
        Route::Passthrough { model, rest } => {
            let query = req.uri().query().map(str::to_owned);
            let path = query.map_or_else(|| rest.clone(), |q| format!("{rest}?{q}"));
            let (parts, body) = match read_body(cfg, req, family).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let req =
                ForwardRequest { method: parts.method, path_and_query: &path, headers: &parts.headers, body, family };
            admit_and_forward(st, cfg, &model, req).await
        }
        Route::Model { .. } | Route::Native => {
            let path_and_query =
                req.uri().path_and_query().map_or_else(|| req.uri().path().to_owned(), ToString::to_string);
            let (parts, body) = match read_body(cfg, req, family).await {
                Ok(v) => v,
                Err(resp) => return resp,
            };
            let hints = body_hints(&body);
            let model = match resolve_model(cfg, hints.model) {
                Ok(m) => m,
                Err(msg) => {
                    return refuse(family, StatusCode::BAD_REQUEST, "model_required", &msg, "", &endpoint);
                }
            };
            debug!(model = %model, path = %path_and_query, stream = hints.stream, "request");
            let req = ForwardRequest {
                method: parts.method,
                path_and_query: &path_and_query,
                headers: &parts.headers,
                body,
                family,
            };
            admit_and_forward(st, cfg, &model, req).await
        }
    }
}

/// Chooses the model name for a request (spec §8.2 last paragraph).
fn resolve_model(cfg: &Config, requested: Option<String>) -> Result<String, String> {
    if let Some(m) = requested.filter(|m| !m.is_empty()) {
        return Ok(m);
    }
    if cfg.server.require_model_field {
        return Err("The 'model' field is required.".to_owned());
    }
    cfg.server
        .default_model
        .clone()
        .ok_or_else(|| "The 'model' field is required because no default_model is configured.".to_owned())
}

/// Admission followed by forwarding.
async fn admit_and_forward(st: &ProxyState, cfg: &Config, model: &str, req: ForwardRequest<'_>) -> Response<Body> {
    match st.controller.admit(model).await {
        Ok(admitted) => forward(&st.client, cfg, admitted, req).await,
        Err(rejection) => {
            // A client giving up while it waited is not a failure of the request.
            if rejection.kind != RejectKind::RequestCancelled {
                let endpoint = req.path_and_query.split('?').next().unwrap_or_default();
                log_failure(model, endpoint, status_for(rejection.kind, &cfg.errors).as_u16(), &rejection.message);
            }
            rejection_response(req.family, &rejection, &cfg.errors)
        }
    }
}

/// `GET /health`.
async fn health(st: &ProxyState) -> Response<Body> {
    match st.controller.status().await {
        Some(s) => json_response(StatusCode::OK, &json!({ "status": "ok", "paused": s.paused, "game": s.game }), None),
        None => rejection_response(
            ApiFamily::OpenAi,
            &Rejection::new(RejectKind::ShuttingDown, "", "The gatekeeper is shutting down."),
            &Config::default().errors,
        ),
    }
}

/// `GET /v1/models`: every enabled model plus its gate state (spec §8.2).
async fn model_list(st: &ProxyState, cfg: &Config) -> Response<Body> {
    let status = st.controller.status().await;
    let data: Vec<_> = cfg
        .models
        .iter()
        .filter(|m| m.enabled)
        .map(|m| {
            let rt = status.as_ref().and_then(|s| s.models.iter().find(|x| x.id == m.id));
            let state = rt.map_or(ModelStateKind::Unloaded, |x| x.state);
            let blocked_reason = rt.and_then(|x| x.blocked_reason);
            json!({
                "id": m.id,
                "object": "model",
                "created": 0,
                "owned_by": "vram-gatekeeper",
                "gatekeeper": { "state": state, "blocked_reason": blocked_reason },
            })
        })
        .collect();
    json_response(StatusCode::OK, &json!({ "object": "list", "data": data }), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_stripping() {
        assert_eq!(host_without_port("127.0.0.1:8088"), "127.0.0.1");
        assert_eq!(host_without_port("localhost"), "localhost");
        assert_eq!(host_without_port("[::1]:8088"), "[::1]");
        assert_eq!(host_without_port("evil.example.com:80"), "evil.example.com");
    }

    #[test]
    fn host_allow_list() {
        let mut cfg = Config::default();
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("localhost:8088"));
        assert!(host_allowed(&cfg, &h));
        h.insert(header::HOST, HeaderValue::from_static("attacker.example:8088"));
        assert!(!host_allowed(&cfg, &h), "DNS-rebinding style host is refused");
        cfg.server.allowed_host_names = vec!["my-pc".into()];
        h.insert(header::HOST, HeaderValue::from_static("MY-PC:8088"));
        assert!(host_allowed(&cfg, &h));
    }

    #[test]
    fn api_key_accepts_bearer_and_x_api_key_only_when_correct() {
        let mut h = HeaderMap::new();
        assert!(authorized(None, &h), "no key configured: open");
        assert!(!authorized(Some("secret"), &h));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer secret"));
        assert!(authorized(Some("secret"), &h));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer wrong"));
        assert!(!authorized(Some("secret"), &h));
        h.insert("x-api-key", HeaderValue::from_static("secret"));
        assert!(authorized(Some("secret"), &h));
    }

    #[test]
    fn browser_origin_requires_one_valid_approved_value() {
        let mut cfg = Config::default();
        let mut headers = HeaderMap::new();
        assert_eq!(checked_origin(&cfg, &headers), Ok(None), "native clients remain allowed");
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://tauri.localhost"));
        assert!(checked_origin(&cfg, &headers).is_err(), "default denies web origins");
        cfg.server.cors_allowed_origins = vec!["http://tauri.localhost".into()];
        assert!(checked_origin(&cfg, &headers).is_ok());
        headers.insert(header::ORIGIN, HeaderValue::from_static("http://tauri.localhost:5173"));
        assert!(checked_origin(&cfg, &headers).is_err(), "port is part of exact origin identity");

        cfg.server.cors_allowed_origins = vec!["*".into()];
        for valid in ["https://app.example", "http://localhost:5173", "http://[::1]:8088", "tauri://localhost", "null"]
        {
            headers.insert(header::ORIGIN, HeaderValue::from_str(valid).expect("header"));
            assert!(checked_origin(&cfg, &headers).is_ok(), "explicit wildcard accepts {valid}");
        }
        for malformed in [
            "",
            "*",
            "localhost",
            "https://",
            "https://user@app.example",
            "https://app.example/",
            "https://app.example/path",
            "https://app.example?query",
            "https://app.example#fragment",
            "https://app.example,https://other.example",
            "https://app.example https://other.example",
            "https://app.example\\other",
            "https://app.example:bad",
            " null",
            "null ",
        ] {
            headers.insert(header::ORIGIN, HeaderValue::from_str(malformed).expect("header"));
            assert!(checked_origin(&cfg, &headers).is_err(), "reject malformed {malformed:?} even with wildcard");
        }
        headers.insert(header::ORIGIN, HeaderValue::from_bytes(b"https://app.example\xff").expect("header"));
        assert!(checked_origin(&cfg, &headers).is_err(), "non-ASCII origin is malformed");
        headers.insert(header::ORIGIN, HeaderValue::from_static("https://app.example"));
        headers.append(header::ORIGIN, HeaderValue::from_static("https://app.example"));
        assert!(checked_origin(&cfg, &headers).is_err(), "duplicate headers are never a single origin");
    }

    #[test]
    fn model_resolution_follows_settings() {
        let mut cfg = Config::default();
        assert!(resolve_model(&cfg, None).is_err());
        cfg.server.default_model = Some("a".into());
        assert_eq!(resolve_model(&cfg, None).as_deref(), Ok("a"));
        assert_eq!(resolve_model(&cfg, Some("b".into())).as_deref(), Ok("b"));
        cfg.server.require_model_field = true;
        assert!(resolve_model(&cfg, None).is_err());
    }
}
