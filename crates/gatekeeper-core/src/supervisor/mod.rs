//! Management of llama-server child processes (spec §9).
//!
//! Responsibility: helpers for launching, observing and measuring children.
//! Not responsible for: deciding when to start or stop (see `gate`).

pub mod footprint;
pub mod health;
pub mod launcher;
pub mod log_capture;
pub mod log_files;
pub mod log_parse;
pub mod ports;
