//! Warn-level logging of failed requests (spec §20).
//!
//! Responsibility: make a failed request visible at the default log level, with
//! the model, endpoint, status and the first 300 characters of the error
//! message. Covers the gatekeeper's own refusals ([`refuse`]) and error
//! responses from llama-server ([`FailureTap`], which reads the body as it
//! passes by without delaying or changing it).
//! Not responsible for: successful requests (logged at `debug` by the server)
//! or for choosing the error format (see `errors`).
//!
//! Request and response bodies are never logged; only the truncated error
//! message of a failed response is.

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use axum::http::{Response, StatusCode};
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use tracing::warn;

use super::cancel_body::BodyError;
use super::errors::{ApiFamily, simple_error};

/// Characters of an error message that reach the log.
pub const MESSAGE_LOG_CHARS: usize = 300;

/// Bytes of an error body kept to find its message. Larger bodies are cut
/// here for the log only; the client still receives every byte.
const PEEK_BYTES: usize = 64 * 1024;

/// Cuts `text` to at most `max` characters, never inside a character, and
/// marks the cut with an ellipsis.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}

/// Logs one failed request at `warn`.
pub fn log_failure(model: &str, endpoint: &str, status: u16, message: &str) {
    warn!(model, endpoint, status, error = %truncate_chars(message.trim(), MESSAGE_LOG_CHARS), "request failed");
}

/// Logs a refusal made by the gatekeeper itself and builds the error response.
pub fn refuse(
    family: ApiFamily,
    status: StatusCode,
    code: &str,
    message: &str,
    model: &str,
    endpoint: &str,
) -> Response<Body> {
    log_failure(model, endpoint, status.as_u16(), message);
    simple_error(family, status, code, message)
}

/// The human message inside an error body: `error.message` (OpenAI, llama.cpp,
/// Anthropic), a plain `message`, or the text itself. Bytes that are not
/// UTF-8 are replaced, never rejected.
fn error_message(body: &[u8]) -> String {
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) {
        let found = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .or_else(|| value.get("error"))
            .and_then(serde_json::Value::as_str);
        if let Some(message) = found {
            return message.to_owned();
        }
    }
    String::from_utf8_lossy(body).into_owned()
}

/// What is known about the failed response being tapped.
pub struct FailureContext {
    /// Model id.
    pub model: String,
    /// Request path without the query.
    pub endpoint: String,
    /// Upstream status.
    pub status: u16,
}

pin_project_lite::pin_project! {
    /// Passes an error response through unchanged and logs it once: when the
    /// body ends, fails, or is dropped (the client hung up).
    pub struct FailureTap<B> {
        #[pin]
        inner: B,
        seen: Vec<u8>,
        context: Option<FailureContext>,
    }

    impl<B> PinnedDrop for FailureTap<B> {
        fn drop(this: Pin<&mut Self>) {
            let this = this.project();
            finish(this.context, this.seen);
        }
    }
}

impl<B> FailureTap<B> {
    /// Wraps `inner`, an upstream response with status 400 or higher.
    pub fn new(inner: B, context: FailureContext) -> Self {
        Self { inner, seen: Vec::new(), context: Some(context) }
    }
}

/// Logs the failure the first time it is called for `context`.
fn finish(context: &mut Option<FailureContext>, seen: &[u8]) {
    if let Some(c) = context.take() {
        log_failure(&c.model, &c.endpoint, c.status, &error_message(seen));
    }
}

impl<B> HttpBody for FailureTap<B>
where
    B: HttpBody<Data = Bytes, Error = BodyError>,
{
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let mut this = self.project();
        let polled = this.inner.as_mut().poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    let room = PEEK_BYTES.saturating_sub(this.seen.len());
                    this.seen.extend_from_slice(&data[..data.len().min(room)]);
                }
            }
            Poll::Ready(None | Some(Err(_))) => finish(this.context, this.seen),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_chars_cuts_on_character_boundaries() {
        assert_eq!(truncate_chars("short", 300), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc…");
        // Multi-byte characters count once each and are never split.
        assert_eq!(truncate_chars("ééééé", 2), "éé…");
        assert_eq!(truncate_chars("日本語のテキスト", 3), "日本語…");
        assert_eq!(truncate_chars("", 3), "");
    }

    #[test]
    fn the_message_is_found_in_the_usual_shapes() {
        assert_eq!(error_message(br#"{"error":{"code":500,"message":"bad template","type":"x"}}"#), "bad template");
        assert_eq!(error_message(br#"{"message":"plain"}"#), "plain");
        assert_eq!(error_message(br#"{"error":"flat"}"#), "flat");
        assert_eq!(error_message(b"upstream exploded"), "upstream exploded");
        assert_eq!(error_message(b""), "");
        assert_eq!(error_message(&[0xff, 0xfe, b'o', b'k']), "\u{fffd}\u{fffd}ok", "invalid UTF-8 is replaced");
    }
}
