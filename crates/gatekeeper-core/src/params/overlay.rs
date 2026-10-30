//! Curated hints merged onto the parsed catalog (spec §10.2 step 4).
//!
//! Responsibility: mark reserved parameters, force float typing where the
//! help text says `N`, and flag the "basic" subset for the UI. The overlay
//! only *annotates* parameters that exist; it never invents flags.
//! Not responsible for: parsing help output (`help_parser`).
//!
//! Implements FR-MOD-1 and the reserved-parameter rule of spec §10.3.

use std::collections::HashSet;
use std::sync::OnceLock;

use serde::Deserialize;

use super::catalog::{ParamCatalog, ValueKind};

/// Overlay data shipped with the app (`overlay.json`).
const OVERLAY_JSON: &str = include_str!("overlay.json");

/// Parsed form of `overlay.json`.
#[derive(Debug, Deserialize)]
struct Overlay {
    /// Parameters the app manages itself.
    reserved: HashSet<String>,
    /// Parameters whose placeholder is `N` but whose value is a float.
    float_keys: HashSet<String>,
    /// Parameters shown in the UI's basic view.
    basic_keys: HashSet<String>,
}

/// Returns the overlay, parsing it once.
fn overlay() -> &'static Overlay {
    static CELL: OnceLock<Overlay> = OnceLock::new();
    CELL.get_or_init(|| {
        // The JSON is a compile-time constant checked by a unit test, so a
        // parse failure here is a programming error, not a runtime one.
        serde_json::from_str(OVERLAY_JSON).expect("overlay.json is valid (see tests)")
    })
}

/// True if `name` (with or without dashes) is a parameter the app manages
/// itself and the user may not set (spec §10.3).
pub fn is_reserved(name: &str) -> bool {
    overlay().reserved.contains(name.trim_start_matches('-'))
}

/// Applies the overlay to a freshly parsed catalog.
pub fn apply(catalog: &mut ParamCatalog) {
    let ov = overlay();
    for p in &mut catalog.params {
        let names: Vec<&str> = p.flags.iter().map(|f| f.trim_start_matches('-')).collect();
        p.reserved = names.iter().any(|n| ov.reserved.contains(*n));
        p.basic = names.iter().any(|n| ov.basic_keys.contains(*n));
        if p.value_kind == ValueKind::Integer && names.iter().any(|n| ov.float_keys.contains(*n)) {
            p.value_kind = ValueKind::Float;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_json_parses() {
        assert!(is_reserved("--port"));
        assert!(!is_reserved("--ctx-size"));
    }
}
