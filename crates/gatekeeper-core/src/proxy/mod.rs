//! The OpenAI-compatible request proxy (spec §8).
//!
//! Responsibility: accept client requests, ask the gate controller for
//! admission, and forward to llama-server.
//! Not responsible for: lifecycle decisions (see `gate`).

pub mod admin_api;
pub mod cancel_body;
pub mod errors;
pub mod failure_log;
pub mod forward;
pub mod inflight;
pub mod routing;
pub mod server;

#[cfg(test)]
mod proxy_tests;
