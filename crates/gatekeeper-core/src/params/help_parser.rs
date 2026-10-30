//! Parser for `llama-server --help` output (spec §10.2).
//!
//! Responsibility: turn the help text of the installed engine into a
//! [`ParamCatalog`]. The parser is deliberately forgiving: a line it cannot
//! classify still yields a free-text parameter, so no flag is ever hidden
//! (spec §27, risk "`--help` format changes").
//! Not responsible for: running the binary (see `engine::install`) or UI
//! hints (see `overlay`).
//!
//! Help layout, as printed by llama.cpp:
//!
//! ```text
//! ----- common params -----
//!
//! -t,    --threads N                      number of CPU threads (default: -1)
//!                                         (env: LLAMA_ARG_THREADS)
//! --mmap, --no-mmap                       whether to memory-map the model
//! ```
//!
//! An entry starts at column 0 with `-`; its flag list and the description
//! are separated by two or more spaces; continuation lines are indented.
//!
//! Implements FR-MOD-1 and decision D8.

use super::catalog::{ParamCatalog, ParamSpec, ValueKind};
use super::overlay;

/// Phrases that mark a flag as repeatable in its description.
const REPEAT_HINTS: [&str; 3] = ["multiple times", "can be repeated", "may be repeated"];

/// Parses help text into a catalog, applies the overlay, and stamps it with
/// the engine version and generation time.
pub fn parse_help(help: &str, engine_version: &str, generated_at: &str) -> ParamCatalog {
    let mut params: Vec<ParamSpec> = Vec::new();
    let mut section = String::from("common");
    // Index of the entry that continuation lines belong to.
    let mut current: Option<usize> = None;

    for raw in help.lines() {
        let line = raw.trim_end();
        if line.trim().is_empty() {
            current = None;
            continue;
        }
        if let Some(name) = section_header(line) {
            section = name;
            current = None;
            continue;
        }
        if line.starts_with('-') {
            if let Some(spec) = parse_entry(line, &section) {
                params.push(spec);
                current = Some(params.len() - 1);
            } else {
                current = None;
            }
        } else if line.starts_with(' ') {
            // Continuation of the previous entry's description.
            if let Some(i) = current {
                append_description(&mut params[i], line.trim());
            }
        }
    }

    for p in &mut params {
        finish_spec(p);
    }

    let mut catalog =
        ParamCatalog { engine_version: engine_version.to_owned(), generated_at: generated_at.to_owned(), params };
    overlay::apply(&mut catalog);
    catalog
}

/// Recognises `----- common params -----` style headers.
fn section_header(line: &str) -> Option<String> {
    let t = line.trim();
    let inner = t.strip_prefix("-----")?.strip_suffix("-----")?.trim();
    // "common params" -> "common"
    Some(inner.trim_end_matches(" params").trim().to_owned())
}

/// Parses the first line of an entry. Returns `None` for lines that start
/// with `-` but contain no flag (for example a wrapped description).
fn parse_entry(line: &str, section: &str) -> Option<ParamSpec> {
    let normalised = collapse_after_commas(line);
    // The flag list ends at the first run of two or more spaces.
    let (left, description) = match find_double_space(&normalised) {
        Some(i) => (&normalised[..i], normalised[i..].trim()),
        None => (normalised.as_str(), ""),
    };

    let mut flags: Vec<String> = Vec::new();
    let mut placeholder: Option<String> = None;
    let items: Vec<&str> = left.split(", ").map(str::trim).collect();
    for (idx, item) in items.iter().enumerate() {
        if !item.starts_with('-') {
            return None;
        }
        // Only the last item can carry a value placeholder.
        if idx + 1 == items.len() {
            if let Some((flag, rest)) = item.split_once(' ') {
                flags.push(flag.to_owned());
                let rest = rest.trim();
                if !rest.is_empty() {
                    placeholder = Some(rest.to_owned());
                }
            } else {
                flags.push((*item).to_owned());
            }
        } else {
            flags.push((*item).to_owned());
        }
    }
    if flags.is_empty() {
        return None;
    }

    let key = flags
        .iter()
        .find(|f| f.starts_with("--"))
        .or_else(|| flags.first())
        .map(|f| f.trim_start_matches('-').to_owned())?;

    Some(ParamSpec {
        key,
        flags,
        value_kind: ValueKind::Flag,
        placeholder,
        choices: None,
        default: None,
        env: None,
        section: section.to_owned(),
        description: description.to_owned(),
        negation_flag: None,
        repeatable: false,
        basic: false,
        reserved: false,
    })
}

/// Replaces "`,` followed by spaces" with a single ", " so flag lists such
/// as `-t,    --threads N` split uniformly.
fn collapse_after_commas(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == ',' && chars.peek() == Some(&' ') {
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            out.push(' ');
        }
    }
    out
}

/// Byte index of the first run of two or more spaces, if any.
fn find_double_space(s: &str) -> Option<usize> {
    s.find("  ")
}

/// Appends a continuation line to an entry's description.
fn append_description(spec: &mut ParamSpec, text: &str) {
    if !spec.description.is_empty() {
        spec.description.push(' ');
    }
    spec.description.push_str(text);
}

/// Derives type, default, env and toggle information once the whole
/// description has been collected.
fn finish_spec(p: &mut ParamSpec) {
    // Pull the env variable out of the description.
    if let Some(start) = p.description.find("(env:") {
        let rest = &p.description[start + "(env:".len()..];
        if let Some(end) = rest.find(')') {
            p.env = Some(rest[..end].trim().to_owned());
            let mut cleaned = p.description[..start].trim_end().to_owned();
            cleaned.push_str(rest[end + 1..].trim_start());
            p.description = cleaned.trim().to_owned();
        }
    }

    p.default = extract_default(&p.description);
    p.repeatable = REPEAT_HINTS.iter().any(|h| p.description.to_lowercase().contains(h));

    // Toggle: a `--no-x` flag whose positive form is also listed.
    let negation = p
        .flags
        .iter()
        .find(|f| f.starts_with("--no-") && p.flags.iter().any(|g| **g == format!("--{}", &f["--no-".len()..])))
        .cloned();
    if let Some(neg) = negation {
        p.negation_flag = Some(neg.clone());
        // Make the positive spelling the key so the file has one name.
        if let Some(pos) = p.flags.iter().find(|f| f.starts_with("--") && **f != neg) {
            p.key = pos.trim_start_matches('-').to_owned();
        }
        p.value_kind = ValueKind::Toggle;
        return;
    }

    let Some(ph) = p.placeholder.clone() else {
        p.value_kind = ValueKind::Flag;
        return;
    };

    if let Some(choices) = parse_choices(&ph).or_else(|| allowed_values(&p.description)) {
        p.choices = Some(choices);
        p.value_kind = ValueKind::Enum;
    } else {
        p.value_kind = kind_from_placeholder(&ph, p.default.as_deref());
    }
    if p.repeatable {
        p.value_kind = ValueKind::List;
    }
}

/// Extracts `[a|b|c]`, `<a|b>` or `{a,b,c}` choice lists.
fn parse_choices(placeholder: &str) -> Option<Vec<String>> {
    let (open, close, sep) = match placeholder.chars().next()? {
        '[' => ('[', ']', '|'),
        '<' => ('<', '>', '|'),
        '{' => ('{', '}', ','),
        _ => return None,
    };
    let inner = placeholder.strip_prefix(open)?.strip_suffix(close)?;
    let choices: Vec<String> = inner.split(sep).map(|s| s.trim().to_owned()).collect();
    (choices.len() > 1 && choices.iter().all(|c| !c.is_empty() && !c.contains(' '))).then_some(choices)
}

/// Reads an `allowed values: a, b, c` sentence from a description, as
/// printed for cache types (`--cache-type-k`).
fn allowed_values(description: &str) -> Option<Vec<String>> {
    let idx = description.find("allowed values:")?;
    let rest = &description[idx + "allowed values:".len()..];
    // The list ends where the next parenthesised note begins.
    let end = rest.find(" (").unwrap_or(rest.len());
    let choices: Vec<String> = rest[..end].split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect();
    (choices.len() > 1 && choices.iter().all(|c| !c.contains(' '))).then_some(choices)
}

/// Chooses a value kind from the placeholder text and the default.
fn kind_from_placeholder(placeholder: &str, default: Option<&str>) -> ValueKind {
    let upper = placeholder.to_ascii_uppercase();
    let is_path = ["FNAME", "FILE", "PATH", "DIR", "URL"].iter().any(|k| upper.contains(k));
    if is_path {
        return ValueKind::Path;
    }
    // `<0...100>` style ranges are whole numbers.
    if upper.starts_with('<') && upper.contains("...") && !upper.contains('|') {
        return ValueKind::Integer;
    }
    if upper == "N" || upper.starts_with("N,") || upper == "N0" {
        // Some `N` options also take a word, e.g. `--gpu-layers` (default:
        // auto). A single-word default is the signal; a free-text default such
        // as "same as --threads" still means a number.
        if default.is_some_and(is_single_word) {
            return ValueKind::String;
        }
        // The help text says "N" for floats too; a decimal point in the
        // default is the best available signal. The overlay fixes the rest.
        return if default.is_some_and(|d| d.contains('.')) { ValueKind::Float } else { ValueKind::Integer };
    }
    ValueKind::String
}

/// True for a lone alphabetic word such as `auto` (not a number or phrase).
fn is_single_word(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphabetic())
}

/// Finds the text after the last `default:` marker, trimmed of quotes and
/// cut at the first comma or closing parenthesis.
fn extract_default(description: &str) -> Option<String> {
    let idx = description.rfind("default:")?;
    let rest = &description[idx + "default:".len()..];
    let end = rest.find([',', ')']).unwrap_or(rest.len());
    let value = rest[..end].trim().trim_matches('\'').trim_matches('"').trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
----- common params -----

-h,    --help, --usage                  print usage and exit
-t,    --threads N                      number of CPU threads to use during generation (default: -1)
                                        (env: LLAMA_ARG_THREADS)
-c,    --ctx-size N                     size of the prompt context (default: 0, 0 = loaded from model)
                                        (env: LLAMA_ARG_CTX_SIZE)
-fa,   --flash-attn [on|off|auto]       set Flash Attention use ('on', 'off', or 'auto', default: 'auto')
                                        (env: LLAMA_ARG_FLASH_ATTN)
--mmap, --no-mmap                       whether to memory-map model (default: enabled)
-m,    --model FNAME                    model path
--temp N                                temperature (default: 0.8)
--lora FNAME                            path to LoRA adapter (can be repeated to use multiple adapters)

----- server params -----

--no-warmup                             skip warming up the model
-sm,   --split-mode {none,layer,row}    how to split the model across GPUs
";

    fn catalog() -> ParamCatalog {
        parse_help(SAMPLE, "test", "2026-01-01T00:00:00Z")
    }

    #[test]
    fn parses_integer_with_env_and_default() {
        let c = catalog();
        let p = c.resolve("ctx-size").expect("ctx-size parsed");
        assert_eq!(p.value_kind, ValueKind::Integer);
        assert_eq!(p.flags, ["-c", "--ctx-size"]);
        assert_eq!(p.default.as_deref(), Some("0"));
        assert_eq!(p.env.as_deref(), Some("LLAMA_ARG_CTX_SIZE"));
        assert!(!p.description.contains("env:"));
    }

    #[test]
    fn parses_enum_choices() {
        let c = catalog();
        let p = c.resolve("flash-attn").expect("flash-attn parsed");
        assert_eq!(p.value_kind, ValueKind::Enum);
        assert_eq!(p.choices.as_deref(), Some(&["on".to_owned(), "off".to_owned(), "auto".to_owned()][..]));
        assert_eq!(p.default.as_deref(), Some("auto"));
        let split = c.resolve("split-mode").expect("split-mode parsed");
        assert_eq!(split.choices.as_ref().map(Vec::len), Some(3));
    }

    #[test]
    fn parses_toggle_pair_with_positive_key() {
        let c = catalog();
        let p = c.resolve("mmap").expect("mmap parsed");
        assert_eq!(p.value_kind, ValueKind::Toggle);
        assert_eq!(p.key, "mmap");
        assert_eq!(p.negation_flag.as_deref(), Some("--no-mmap"));
    }

    #[test]
    fn lone_negative_flag_is_a_plain_flag() {
        let c = catalog();
        let p = c.resolve("no-warmup").expect("no-warmup parsed");
        assert_eq!(p.value_kind, ValueKind::Flag);
        assert_eq!(p.section, "server");
    }

    #[test]
    fn float_detected_from_default_and_overlay() {
        let c = catalog();
        assert_eq!(c.resolve("temp").expect("temp parsed").value_kind, ValueKind::Float);
    }

    #[test]
    fn repeatable_flag_becomes_list() {
        let c = catalog();
        assert_eq!(c.resolve("lora").expect("lora parsed").value_kind, ValueKind::List);
    }

    #[test]
    fn reserved_and_basic_flags_are_marked() {
        let c = catalog();
        assert!(c.resolve("model").expect("model parsed").reserved);
        assert!(c.resolve("ctx-size").expect("ctx-size parsed").basic);
    }

    #[test]
    fn resolves_by_any_alias() {
        let c = catalog();
        assert!(c.resolve("-c").is_some());
        assert!(c.resolve("--threads").is_some());
        assert!(c.resolve("usage").is_some());
        assert!(c.resolve("does-not-exist").is_none());
    }
}
