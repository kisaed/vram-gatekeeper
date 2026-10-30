//! Endpoint table and model extraction (spec §8.2, §8.3).
//!
//! Responsibility: decide what a request path means and pull `model` and
//! `stream` out of a JSON body without touching the bytes that get
//! forwarded.
//! Not responsible for: admission or forwarding.
//!
//! Implements FR-REQ-1, FR-REQ-2.

use serde::Deserialize;

use super::errors::ApiFamily;

/// What a request path asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Inference endpoint whose body carries `model`.
    Model {
        /// Error format family.
        family: ApiFamily,
    },
    /// Native llama.cpp endpoint: `model` if present, else the default model.
    Native,
    /// `GET /v1/models`, answered by the proxy.
    ModelList,
    /// `/m/{model_id}/{rest}`: explicit passthrough.
    Passthrough {
        /// Model id from the path.
        model: String,
        /// Path to forward, starting with `/`.
        rest: String,
    },
    /// The gatekeeper's own `/health`.
    Health,
    /// `/gatekeeper/v1/*` admin endpoints; carries the sub-path.
    Admin(String),
    /// Unknown path.
    NotFound,
}

/// Inference endpoints with a `model` field (spec §8.2).
const OPENAI_MODEL_PATHS: [&str; 6] =
    ["/v1/chat/completions", "/v1/completions", "/v1/embeddings", "/v1/responses", "/v1/rerank", "/v1/reranking"];

/// Anthropic-style endpoints.
const ANTHROPIC_PATHS: [&str; 2] = ["/v1/messages", "/v1/messages/count_tokens"];

/// llama.cpp native endpoints.
const NATIVE_PATHS: [&str; 6] = ["/completion", "/infill", "/tokenize", "/detokenize", "/apply-template", "/embedding"];

/// Classifies a request.
pub fn classify(method: &str, path: &str) -> Route {
    if path == "/health" && method == "GET" {
        return Route::Health;
    }
    if let Some(rest) = path.strip_prefix("/gatekeeper/v1/") {
        return Route::Admin(rest.to_owned());
    }
    if path == "/v1/models" && method == "GET" {
        return Route::ModelList;
    }
    if let Some(rest) = path.strip_prefix("/m/") {
        if let Some((model, tail)) = rest.split_once('/')
            && !model.is_empty()
        {
            return Route::Passthrough { model: model.to_owned(), rest: format!("/{tail}") };
        }
        return Route::NotFound;
    }
    if method == "POST" {
        if OPENAI_MODEL_PATHS.contains(&path) {
            return Route::Model { family: ApiFamily::OpenAi };
        }
        if ANTHROPIC_PATHS.contains(&path) {
            return Route::Model { family: ApiFamily::Anthropic };
        }
        if NATIVE_PATHS.contains(&path) {
            return Route::Native;
        }
    }
    Route::NotFound
}

/// The only fields the proxy reads from a request body (spec §8.3).
#[derive(Debug, Default, Deserialize)]
pub struct BodyHints {
    /// Requested model name.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether the client asked for a streamed response.
    #[serde(default)]
    pub stream: bool,
}

/// Extracts hints from a JSON body; unparseable bodies yield defaults so the
/// upstream can produce its own error.
pub fn body_hints(body: &[u8]) -> BodyHints {
    serde_json::from_slice(body).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_endpoints_route_by_family() {
        assert_eq!(classify("POST", "/v1/chat/completions"), Route::Model { family: ApiFamily::OpenAi });
        assert_eq!(classify("POST", "/v1/messages"), Route::Model { family: ApiFamily::Anthropic });
        assert_eq!(classify("POST", "/v1/messages/count_tokens"), Route::Model { family: ApiFamily::Anthropic });
        assert_eq!(classify("POST", "/v1/reranking"), Route::Model { family: ApiFamily::OpenAi });
    }

    #[test]
    fn native_endpoints_and_methods() {
        assert_eq!(classify("POST", "/completion"), Route::Native);
        assert_eq!(classify("GET", "/completion"), Route::NotFound);
        assert_eq!(classify("GET", "/v1/models"), Route::ModelList);
        assert_eq!(classify("POST", "/v1/models"), Route::NotFound);
    }

    #[test]
    fn passthrough_splits_model_and_rest() {
        assert_eq!(
            classify("GET", "/m/qwen3-8b/props"),
            Route::Passthrough { model: "qwen3-8b".into(), rest: "/props".into() }
        );
        assert_eq!(classify("GET", "/m/"), Route::NotFound);
        assert_eq!(classify("GET", "/m/onlymodel"), Route::NotFound);
    }

    #[test]
    fn health_and_admin() {
        assert_eq!(classify("GET", "/health"), Route::Health);
        assert_eq!(classify("POST", "/gatekeeper/v1/pause"), Route::Admin("pause".into()));
        assert_eq!(classify("GET", "/nope"), Route::NotFound);
    }

    #[test]
    fn hints_read_model_and_stream_only() {
        let h = body_hints(br#"{"model":"qwen3","stream":true,"messages":[{"role":"user","content":"hi"}]}"#);
        assert_eq!(h.model.as_deref(), Some("qwen3"));
        assert!(h.stream);
        let none = body_hints(b"not json");
        assert!(none.model.is_none() && !none.stream);
        assert!(body_hints(br#"{"messages":[]}"#).model.is_none());
    }
}
