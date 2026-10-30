//! `/gatekeeper/v1/*` admin endpoints (spec §8.7).
//!
//! Responsibility: let scripts query status and control models, the gate and
//! pausing through HTTP. Authentication and the on/off switch are enforced
//! by the server before a request gets here.
//! Not responsible for: the controller logic behind each action.
//!
//! Implements FR-APP-3.

use axum::body::Body;
use axum::http::{Method, Response, StatusCode};
use serde::Deserialize;
use serde_json::json;

use super::errors::{ApiFamily, json_response, rejection_response, simple_error};
use crate::config::schema::ErrorsConfig;
use crate::gate::controller::ControllerHandle;

/// Body of `POST /models/{id}/unload`.
#[derive(Debug, Default, Deserialize)]
struct UnloadBody {
    /// Reject requests until released.
    #[serde(default)]
    hold: bool,
}

/// Handles one admin request. `sub` is the path after `/gatekeeper/v1/`.
pub async fn handle(
    ctl: &ControllerHandle,
    errors: &ErrorsConfig,
    method: &Method,
    sub: &str,
    body: &[u8],
) -> Response<Body> {
    let ok = || json_response(StatusCode::OK, &json!({ "ok": true }), None);
    let parts: Vec<&str> = sub.trim_end_matches('/').split('/').collect();

    match (method.as_str(), parts.as_slice()) {
        ("GET", ["status"]) => match ctl.status().await {
            Some(status) => json_response(StatusCode::OK, &serde_json::to_value(status).unwrap_or_default(), None),
            None => simple_error(
                ApiFamily::OpenAi,
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The gatekeeper is shutting down.",
            ),
        },
        ("POST", ["models", id, "load"]) => match ctl.load(id).await {
            Ok(()) => ok(),
            Err(r) => rejection_response(ApiFamily::OpenAi, &r, errors),
        },
        ("POST", ["models", id, "unload"]) => {
            let hold = serde_json::from_slice::<UnloadBody>(body).unwrap_or_default().hold;
            ctl.unload(id, hold).await;
            ok()
        }
        ("POST", ["models", id, "release"]) => {
            ctl.release_hold(id);
            ok()
        }
        ("POST", ["override"]) => {
            ctl.allow_anyway();
            ok()
        }
        ("POST", ["pause"]) => {
            ctl.set_paused(true);
            ok()
        }
        ("POST", ["resume"]) => {
            ctl.set_paused(false);
            ok()
        }
        _ => simple_error(ApiFamily::OpenAi, StatusCode::NOT_FOUND, "not_found", "Unknown admin endpoint."),
    }
}
