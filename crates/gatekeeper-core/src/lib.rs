//! VRAM Gatekeeper core: all logic, no UI framework dependency (spec D2).
//!
//! The crate sits between AI clients and one or more `llama-server`
//! processes. It gates requests while games run or VRAM is short, manages
//! each model's process, and exposes the pieces the Tauri host and the
//! headless CLI both use.
//!
//! Safe Rust everywhere except `platform::windows`, which quarantines the
//! unsafe Win32 calls (spec §23.1).

#![deny(missing_docs)]

pub mod app;
pub mod config;
pub mod detect;
pub mod diagnostics;
pub mod engine;
pub mod error;
pub mod events;
pub mod gate;
pub mod gguf;
pub mod gpu;
pub mod hf;
pub mod params;
pub mod platform;
pub mod ports;
pub mod proxy;
pub mod speculative;
pub mod state_store;
pub mod supervisor;
pub mod sync;

pub use error::{CoreError, CoreResult};

#[cfg(test)]
mod app_tests;
