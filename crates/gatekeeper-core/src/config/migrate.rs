//! `schema_version` upgrades for `config.json` (spec §14.6).
//!
//! Responsibility: bring an older config document up to the current layout
//! before it is deserialized. One function per version step.
//! Not responsible for: backing up the original file (the store does that
//! before saving a migrated document).
//!
//! Implements FR-MOD-2.

use serde_json::Value;

use super::defaults::SCHEMA_VERSION;
use crate::error::{CoreError, CoreResult};

/// Result of a migration attempt.
#[derive(Debug)]
pub struct Migrated {
    /// The upgraded document.
    pub value: Value,
    /// The version the file had before migration, if it changed.
    pub from_version: Option<u32>,
}

/// Upgrades `value` to [`SCHEMA_VERSION`].
///
/// A missing `schema_version` is treated as version 1, because hand-written
/// files commonly omit it.
///
/// # Errors
/// Returns [`CoreError::ConfigTooNew`] when the file claims a version newer
/// than this build understands; loading it could silently drop settings.
pub fn migrate(mut value: Value) -> CoreResult<Migrated> {
    let found = value.get("schema_version").and_then(Value::as_u64).map_or(1, |v| u32::try_from(v).unwrap_or(u32::MAX));

    if found > SCHEMA_VERSION {
        return Err(CoreError::ConfigTooNew { found, supported: SCHEMA_VERSION });
    }

    // Future steps (`v1_to_v2`, ...) are applied here in order, one version at
    // a time, and must document what changed. Version 1 is the first release,
    // so no step exists yet, and nothing can be below it because a missing
    // version is read as 1.
    debug_assert_eq!(found, SCHEMA_VERSION, "a migration step is required for version {found}");

    let from_version = (found != SCHEMA_VERSION).then_some(found);
    if let Some(obj) = value.as_object_mut() {
        obj.entry("schema_version").or_insert_with(|| Value::from(SCHEMA_VERSION));
    }
    Ok(Migrated { value, from_version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_version_is_treated_as_current() {
        let m = migrate(serde_json::json!({})).expect("migrates");
        assert_eq!(m.from_version, None);
        assert_eq!(m.value["schema_version"], SCHEMA_VERSION);
    }

    #[test]
    fn newer_version_is_rejected() {
        let err = migrate(serde_json::json!({ "schema_version": SCHEMA_VERSION + 1 }));
        assert!(matches!(err, Err(CoreError::ConfigTooNew { .. })));
    }
}
