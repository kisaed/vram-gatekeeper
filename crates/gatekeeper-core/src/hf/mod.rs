//! Hugging Face search, downloads and the model library (spec §15).
//!
//! Responsibility: find GGUF models on Hugging Face, download them reliably,
//! and manage the files on disk.
//! Not responsible for running models (see `gate`, `supervisor`).

pub mod api;
pub mod download;
pub mod draft_search;
pub mod gguf_files;
pub mod library;
pub mod queue;
pub mod update;
pub mod view;

#[cfg(test)]
pub(crate) mod hf_tests;
