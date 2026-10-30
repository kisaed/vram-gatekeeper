//! Response body wrapper that keeps a request counted and can be cancelled
//! mid-stream (spec §8.5, §8.6).
//!
//! Responsibility: forward upstream frames unchanged until the controller
//! cancels the request, then deliver a clean error and close. Owning the
//! [`InflightGuard`] means the request counter is released when the body is
//! dropped for any reason, including client disconnects.
//! Not responsible for: choosing to cancel (see `gate::controller`).
//!
//! Dropping this body drops the upstream connection, which makes
//! llama-server stop generating (spec F9).
//!
//! Implements FR-GAME-3, FR-REQ-5.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body::{Body, Frame};

use super::errors::{ApiFamily, stream_error_chunk};
use super::inflight::{CancelHandle, InflightGuard};
use crate::config::schema::ErrorsConfig;

/// Boxed error type for body failures.
pub type BodyError = Box<dyn std::error::Error + Send + Sync>;

/// Boxed future that resolves when the request is cancelled.
type CancelFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Where the wrapper is in its life.
enum Phase {
    /// Forwarding upstream frames.
    Running,
    /// Cancelled: the error chunk is queued to be sent next.
    SendError(Bytes),
    /// Finished (normally, or after the error chunk).
    Done,
}

/// How a cancelled response is terminated.
#[derive(Clone)]
pub struct CancelStyle {
    /// Client library family, deciding the error chunk format.
    pub family: ApiFamily,
    /// True if the response is a server-sent event stream. Only then can an
    /// error event be injected; otherwise the connection is aborted.
    pub is_event_stream: bool,
    /// Model id for the error text.
    pub model: String,
    /// Error settings.
    pub errors: ErrorsConfig,
}

pin_project_lite::pin_project! {
    /// Wraps an upstream response body.
    pub struct CancelableBody<B> {
        #[pin]
        inner: B,
        cancel: CancelHandle,
        cancel_fut: CancelFuture,
        style: CancelStyle,
        phase: Phase,
        // Held only for its Drop, which releases the in-flight count.
        _guard: InflightGuard,
    }
}

impl<B> CancelableBody<B> {
    /// Wraps `inner`.
    pub fn new(inner: B, cancel: CancelHandle, guard: InflightGuard, style: CancelStyle) -> Self {
        let waiter = cancel.clone();
        Self {
            inner,
            cancel,
            cancel_fut: Box::pin(async move { waiter.cancelled().await }),
            style,
            phase: Phase::Running,
            _guard: guard,
        }
    }
}

impl<B> Body for CancelableBody<B>
where
    B: Body<Data = Bytes>,
    B::Error: Into<BodyError>,
{
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let mut this = self.project();
        loop {
            match std::mem::replace(this.phase, Phase::Done) {
                Phase::Done => return Poll::Ready(None),
                Phase::SendError(chunk) => {
                    // The error event is the last thing the client receives.
                    return Poll::Ready(Some(Ok(Frame::data(chunk))));
                }
                Phase::Running => {
                    *this.phase = Phase::Running;
                }
            }

            // Cancellation wins over data so a drain is prompt.
            if this.cancel_fut.as_mut().poll(cx).is_ready() {
                let info = this.cancel.reason();
                if this.style.is_event_stream {
                    let chunk = stream_error_chunk(this.style.family, &info, &this.style.model, &this.style.errors);
                    *this.phase = Phase::SendError(chunk);
                    continue;
                }
                // Headers already went out as 200; the only honest signal
                // left is a broken connection.
                *this.phase = Phase::Done;
                return Poll::Ready(Some(Err(info.message.into())));
            }

            return match this.inner.as_mut().poll_frame(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(None) => {
                    *this.phase = Phase::Done;
                    Poll::Ready(None)
                }
                Poll::Ready(Some(Ok(frame))) => Poll::Ready(Some(Ok(frame))),
                Poll::Ready(Some(Err(e))) => {
                    *this.phase = Phase::Done;
                    Poll::Ready(Some(Err(e.into())))
                }
            };
        }
    }

    fn is_end_stream(&self) -> bool {
        matches!(self.phase, Phase::Done) && self.inner.is_end_stream()
    }
}
