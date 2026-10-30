//! Types for the parsed parameter catalog (spec §10.2).
//!
//! Responsibility: describe every llama-server flag the installed engine
//! supports, as plain serializable data the UI can render.
//! Not responsible for: how the catalog is produced (`help_parser`).
//!
//! Implements FR-MOD-1.

use serde::{Deserialize, Serialize};

/// How a parameter's value is typed and rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKind {
    /// Present or absent, no value.
    Flag,
    /// Has a `--x` / `--no-x` pair; stored as a boolean.
    Toggle,
    /// One of a fixed set of choices.
    Enum,
    /// Whole number.
    Integer,
    /// Floating-point number.
    Float,
    /// Free text.
    String,
    /// File or directory path.
    Path,
    /// Repeatable flag taking a value each time.
    List,
}

/// One llama-server parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    /// First long flag without dashes; the canonical `llama_args` key.
    pub key: String,
    /// Every flag spelling, e.g. `["-c", "--ctx-size"]`.
    pub flags: Vec<String>,
    /// Value type.
    pub value_kind: ValueKind,
    /// Placeholder from the help text, e.g. `N` or `FNAME`.
    pub placeholder: Option<String>,
    /// Allowed values for enums.
    pub choices: Option<Vec<String>>,
    /// Default as written in the help text.
    pub default: Option<String>,
    /// Environment variable that also sets this parameter.
    pub env: Option<String>,
    /// Help section the entry appeared under.
    pub section: String,
    /// Description with the env line removed.
    pub description: String,
    /// The `--no-x` flag of a toggle.
    pub negation_flag: Option<String>,
    /// Whether the flag may be given more than once.
    pub repeatable: bool,
    /// Shown in the UI's "Basic" view (set by the overlay).
    #[serde(default)]
    pub basic: bool,
    /// Managed by the app and not user-editable (set by the overlay).
    #[serde(default)]
    pub reserved: bool,
}

/// All parameters of one engine build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ParamCatalog {
    /// Output of `llama-server --version`, or the build id.
    pub engine_version: String,
    /// RFC 3339 timestamp of generation.
    pub generated_at: String,
    /// The parameters.
    pub params: Vec<ParamSpec>,
}

impl ParamCatalog {
    /// Finds a parameter by any of its flag spellings, with or without
    /// leading dashes. Tolerates the `no-` form of a toggle.
    pub fn resolve(&self, name: &str) -> Option<&ParamSpec> {
        let bare = name.trim_start_matches('-');
        self.params.iter().find(|p| p.flags.iter().any(|f| f.trim_start_matches('-') == bare))
    }
}
