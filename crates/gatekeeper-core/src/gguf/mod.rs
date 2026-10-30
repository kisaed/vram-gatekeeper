//! GGUF file inspection (spec §9.5, §15.4).
//!
//! Responsibility: read model metadata from `.gguf` headers.
//! Not responsible for loading models (llama-server does that).

pub mod header;
