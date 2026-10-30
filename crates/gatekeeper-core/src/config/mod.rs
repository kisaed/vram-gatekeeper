//! The single JSON settings file (spec §14).
//!
//! Responsibility: schema types, defaults, validation, migrations and
//! atomic disk access for `config.json`.
//! Not responsible for: applying settings to running components.
//!
//! Implements FR-MOD-2, FR-MOD-3.

pub mod defaults;
pub mod migrate;
pub mod schema;
pub mod store;
pub mod validate;

pub use schema::Config;
pub use validate::{Severity, ValidateOptions, ValidationIssue, errors_of, validate};
