//! Speculative decoding (spec §10.6, FR-MOD-6..9).
//!
//! Responsibility: understand a model's speculative settings. The draft file
//! lives in `files.draft`; the types and their tuning stay in `llama_args`
//! under the engine's own names. This module parses those settings, detects
//! what a draft file is, builds the launch arguments, and reports findings
//! before a model is loaded.
//! Not responsible for: starting the engine (`supervisor::launcher`), the
//! validation framework (`config::validate`) or downloading draft files (`hf`).

pub mod findings;
pub mod types;

pub use findings::{GgufSummary, SpecConfirmation, confirmations, findings, summarize};
pub use types::{
    SpecType, attach_draft, available_types, draft_is_passed, has_mtp_layers, join_types, kind_from_file_name,
    kind_from_meta, launch_args, selected_types, with_type_added,
};
