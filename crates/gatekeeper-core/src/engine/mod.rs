//! llama.cpp engine management (spec §16).
//!
//! Responsibility: find, validate, describe and update the llama-server engine.
//! Not responsible for: running models (see `supervisor`).

pub mod bundled;
pub mod install;
pub mod updater;
