//! `/health` polling against a real llama-server (spec §9.1 step 7).
//!
//! Responsibility: one HTTP check per call, mapped to [`Health`].
//! Not responsible for: the polling loop (see `gate::controller`).

use std::time::Duration;

use crate::ports::{Health, HealthProbe, Upstream};
use crate::proxy::forward::{UpstreamClient, new_client};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};

/// Longest a single health request may take. Health answers instantly even
/// while a model loads, so a slow answer means something is wrong.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Probes `GET /health` over HTTP.
pub struct HttpHealthProbe {
    /// Small dedicated client; probes must not share the proxy's pool.
    client: UpstreamClient,
}

impl Default for HttpHealthProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpHealthProbe {
    /// Creates a probe.
    pub fn new() -> Self {
        Self { client: new_client() }
    }
}

#[async_trait]
impl HealthProbe for HttpHealthProbe {
    async fn check(&self, upstream: &Upstream) -> Health {
        let Ok(req) = Request::builder()
            .uri(format!("http://127.0.0.1:{}/health", upstream.port))
            .header("authorization", format!("Bearer {}", upstream.api_key))
            .body(Body::empty())
        else {
            return Health::Down;
        };
        match tokio::time::timeout(PROBE_TIMEOUT, self.client.request(req)).await {
            Ok(Ok(resp)) => match resp.status() {
                StatusCode::OK => Health::Ready,
                // llama-server answers 503 while the model is loading.
                StatusCode::SERVICE_UNAVAILABLE => Health::Loading,
                _ => Health::Down,
            },
            _ => Health::Down,
        }
    }
}
