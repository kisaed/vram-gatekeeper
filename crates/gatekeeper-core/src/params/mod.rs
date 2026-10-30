//! llama-server parameter handling (spec §10).
//!
//! Responsibility: turn the installed engine's `--help` into a catalog
//! (`help_parser`), merge curated hints (`overlay`), and convert a model's
//! `llama_args` JSON into a command line (`argv`).
//! Not responsible for: spawning processes (see `supervisor`).
//!
//! Implements FR-MOD-1, FR-MOD-5 and decision D8.

pub mod argv;
pub mod catalog;
pub mod help_parser;
pub mod overlay;

pub use argv::{ArgvOutput, ArgvWarning, build_argv, render_command_line};
pub use catalog::{ParamCatalog, ParamSpec, ValueKind};
