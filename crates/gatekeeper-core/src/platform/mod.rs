//! Platform-specific code (spec §6, §23.1).
//!
//! Responsibility: every call into the operating system that needs `unsafe`.
//! The rest of the crate is safe Rust and talks to these wrappers through
//! small safe functions and traits.
//! Not responsible for: policy.

#[cfg(windows)]
pub mod windows;
