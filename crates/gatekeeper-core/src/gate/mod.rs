//! The gate controller and its supporting rules (spec §7, §13).
//!
//! Responsibility: decide, for every request and every event, which models
//! may run, and drive their lifecycle.
//! Not responsible for: HTTP (see `proxy`) or process mechanics (`supervisor`).

pub mod controller;
pub mod drain;
pub mod gate_state;
pub mod loop_guard;
pub mod model_state;
pub mod rejection;
pub mod vram_rules;

#[cfg(test)]
mod controller_props;
#[cfg(test)]
mod controller_tests;
#[cfg(test)]
pub(crate) mod test_support;
