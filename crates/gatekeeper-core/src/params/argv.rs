//! Converts a model's `llama_args` JSON into a command line (spec §10.4).
//!
//! Responsibility: catalog-driven conversion plus the launch preview text.
//! Not responsible for: reserved launch arguments (`--host`, `--port`,
//! `--api-key`, `--model`), which the launcher appends itself.
//!
//! Rules (spec §10.4):
//! - keys not set in `llama_args` are not emitted;
//! - keys unknown to the installed engine are kept in the file, reported as
//!   warnings, and not emitted;
//! - `extra_args` follow verbatim, one argv entry per element;
//! - nothing goes through a shell, so quoting cannot break.
//!
//! Implements FR-MOD-1, FR-MOD-5.

use serde_json::Value;

use super::catalog::{ParamCatalog, ParamSpec, ValueKind};
use super::overlay;
use crate::config::schema::ModelConfig;

/// A non-fatal problem found while building arguments.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ArgvWarning {
    /// The `llama_args` key concerned.
    pub key: String,
    /// Human-readable explanation.
    pub message: String,
}

/// The arguments produced for a model plus any warnings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArgvOutput {
    /// Arguments in order, without the executable.
    pub args: Vec<String>,
    /// Keys that were skipped and why.
    pub warnings: Vec<ArgvWarning>,
}

/// Builds the user-controlled part of a model's command line.
pub fn build_argv(catalog: &ParamCatalog, model: &ModelConfig) -> ArgvOutput {
    let mut out = ArgvOutput::default();

    // BTreeMap iteration is sorted, so the command line is deterministic.
    for (key, value) in &model.llama_args {
        if value.is_null() {
            continue; // `null` means "unset": use llama-server's default.
        }
        if overlay::is_reserved(key) {
            out.warn(key, "managed by Gatekeeper and cannot be set per model");
            continue;
        }
        let Some(spec) = catalog.resolve(key) else {
            out.warn(key, "not supported by the installed engine");
            continue;
        };
        // A parameter is reserved if any of its spellings is, so `m` cannot
        // smuggle in `--model` (the catalog knows `-m` and `--model` are one).
        if spec.reserved {
            out.warn(key, "managed by Gatekeeper and cannot be set per model");
            continue;
        }
        emit(spec, key, value, &mut out);
    }

    out.args.extend(model.extra_args.iter().cloned());
    out
}

impl ArgvOutput {
    /// Records a warning for `key`.
    fn warn(&mut self, key: &str, message: &str) {
        self.warnings.push(ArgvWarning { key: key.to_owned(), message: message.to_owned() });
    }
}

/// Emits the arguments for one parameter.
fn emit(spec: &ParamSpec, written_key: &str, value: &Value, out: &mut ArgvOutput) {
    // Prefer the spelling the user wrote if it is a real flag, so a file that
    // says `n-gpu-layers` produces `--n-gpu-layers`.
    let written = format!("--{}", written_key.trim_start_matches('-'));
    let flag = if spec.flags.contains(&written) { written } else { format!("--{}", spec.key) };

    match spec.value_kind {
        ValueKind::Flag => match value.as_bool() {
            Some(true) => out.args.push(flag),
            Some(false) => {}
            None => out.warn(written_key, "expected true or false"),
        },
        ValueKind::Toggle => match (value.as_bool(), &spec.negation_flag) {
            (Some(true), _) => out.args.push(format!("--{}", spec.key)),
            (Some(false), Some(neg)) => out.args.push(neg.clone()),
            (Some(false), None) => {}
            (None, _) => out.warn(written_key, "expected true or false"),
        },
        ValueKind::List => {
            let items: Vec<&Value> = match value {
                Value::Array(a) => a.iter().collect(),
                other => vec![other],
            };
            for item in items {
                match scalar_to_string(spec, item) {
                    Some(s) => {
                        out.args.push(flag.clone());
                        out.args.push(s);
                    }
                    None => out.warn(written_key, "list items must be strings or numbers"),
                }
            }
        }
        ValueKind::Enum | ValueKind::Integer | ValueKind::Float | ValueKind::String | ValueKind::Path => {
            match scalar_to_string(spec, value) {
                Some(s) => {
                    check_value(spec, written_key, value, &s, out);
                    out.args.push(flag);
                    out.args.push(s);
                }
                None => out.warn(written_key, "expected a string or number"),
            }
        }
    }
}

/// Renders a JSON scalar as an argv string. Booleans map to `on`/`off` for
/// enums that accept them, which is how `flash-attn: true` is usually meant.
fn scalar_to_string(spec: &ParamSpec, value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => {
            let accepts_on_off =
                spec.choices.as_ref().is_some_and(|c| c.iter().any(|x| x == "on") && c.iter().any(|x| x == "off"));
            Some(
                if accepts_on_off {
                    if *b { "on" } else { "off" }
                } else if *b {
                    "true"
                } else {
                    "false"
                }
                .to_owned(),
            )
        }
        _ => None,
    }
}

/// Adds warnings for values that do not match the parameter's type.
fn check_value(spec: &ParamSpec, key: &str, value: &Value, rendered: &str, out: &mut ArgvOutput) {
    match spec.value_kind {
        ValueKind::Integer => {
            let ok = value.as_i64().is_some() || value.as_f64().is_some_and(|f| f.fract() == 0.0)
                || rendered.parse::<i64>().is_ok()
                // llama.cpp accepts words like `auto` or `all` for some integers.
                || value.is_string();
            if !ok {
                out.warn(key, "expected a whole number");
            }
        }
        ValueKind::Float => {
            if value.is_number() || rendered.parse::<f64>().is_ok() {
                return;
            }
            out.warn(key, "expected a number");
        }
        ValueKind::Enum => {
            if let Some(choices) = &spec.choices
                && !choices.iter().any(|c| c == rendered)
            {
                out.warn(key, &format!("expected one of: {}", choices.join(", ")));
            }
        }
        _ => {}
    }
}

/// Renders a command line for display, quoting arguments that need it and
/// masking the value after `--api-key` (spec §10.5 launch preview).
pub fn render_command_line(exe: &str, args: &[String]) -> String {
    let mut parts = vec![quote(exe)];
    let mut mask_next = false;
    for a in args {
        if mask_next {
            parts.push("********".to_owned());
            mask_next = false;
        } else {
            mask_next = a == "--api-key";
            parts.push(quote(a));
        }
    }
    parts.join(" ")
}

/// Quotes an argument for display only; real spawning passes argv directly.
fn quote(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::help_parser::parse_help;

    const HELP: &str = "\
-c,    --ctx-size N                     size of the prompt context (default: 0)
-ngl,  --gpu-layers, --n-gpu-layers N   layers on GPU, number, 'auto' or 'all' (default: auto)
-fa,   --flash-attn [on|off|auto]       flash attention (default: 'auto')
--jinja, --no-jinja                     use jinja templates
--temp N                                temperature (default: 0.8)
--lora FNAME                            LoRA adapter (can be repeated)
--no-warmup                             skip warmup
--port N                                port to listen
";

    fn model(args: &[(&str, Value)]) -> ModelConfig {
        let mut m = ModelConfig::default();
        for (k, v) in args {
            m.llama_args.insert((*k).to_owned(), v.clone());
        }
        m
    }

    fn build(args: &[(&str, Value)]) -> ArgvOutput {
        build_argv(&parse_help(HELP, "t", "t"), &model(args))
    }

    #[test]
    fn integer_and_alias_spelling() {
        let out = build(&[("ctx-size", 16384.into()), ("n-gpu-layers", "auto".into())]);
        assert_eq!(out.args, ["--ctx-size", "16384", "--n-gpu-layers", "auto"]);
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn toggle_true_and_false() {
        assert_eq!(build(&[("jinja", true.into())]).args, ["--jinja"]);
        assert_eq!(build(&[("jinja", false.into())]).args, ["--no-jinja"]);
    }

    #[test]
    fn bool_maps_to_on_off_for_enum() {
        assert_eq!(build(&[("flash-attn", true.into())]).args, ["--flash-attn", "on"]);
    }

    #[test]
    fn invalid_enum_choice_warns_but_still_emits() {
        let out = build(&[("flash-attn", "maybe".into())]);
        assert_eq!(out.args, ["--flash-attn", "maybe"]);
        assert_eq!(out.warnings.len(), 1);
    }

    #[test]
    fn float_value_and_list() {
        let out = build(&[("temp", 0.7.into()), ("lora", serde_json::json!(["a.gguf", "b.gguf"]))]);
        assert_eq!(out.args, ["--lora", "a.gguf", "--lora", "b.gguf", "--temp", "0.7"]);
    }

    #[test]
    fn flag_false_emits_nothing_and_null_is_unset() {
        let out = build(&[("no-warmup", false.into()), ("ctx-size", Value::Null)]);
        assert!(out.args.is_empty());
    }

    #[test]
    fn unknown_and_reserved_keys_are_skipped_with_warnings() {
        let out = build(&[("does-not-exist", 1.into()), ("port", 9.into())]);
        assert!(out.args.is_empty());
        assert_eq!(out.warnings.len(), 2);
    }

    #[test]
    fn extra_args_are_appended_verbatim() {
        let mut m = model(&[("ctx-size", 8.into())]);
        m.extra_args = vec!["--foo".into(), "bar baz".into()];
        let out = build_argv(&parse_help(HELP, "t", "t"), &m);
        assert_eq!(out.args, ["--ctx-size", "8", "--foo", "bar baz"]);
    }

    #[test]
    fn preview_masks_api_key_and_quotes_spaces() {
        let args = vec!["--api-key".into(), "secret".into(), "--model".into(), "C:\\My Models\\a.gguf".into()];
        let line = render_command_line("llama-server.exe", &args);
        assert_eq!(line, "llama-server.exe --api-key ******** --model \"C:\\My Models\\a.gguf\"");
    }
}
