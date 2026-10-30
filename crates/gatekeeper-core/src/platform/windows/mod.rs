//! Thin, commented wrappers over Win32 (spec §23.1).
//!
//! This is the only place in the crate allowed to use `unsafe`. Each
//! wrapper documents the API it calls, links its Microsoft Learn page, and
//! explains how failure is handled.
#![allow(unsafe_code)]

pub mod credentials;
pub mod disk;
pub mod dxgi;
pub mod foreground;
pub mod job_object;
pub mod pdh;
pub mod power;
pub mod process_list;
pub mod registry;
pub mod window_list;

/// Encodes a string as NUL-terminated UTF-16.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
