//! Tests against output captured from the real llama.cpp engine (see
//! `tests/fixtures/README.md`). Unlike the fake server, these cannot agree
//! with the parser by construction, so they guard against format drift.

use gatekeeper_core::config::schema::ModelConfig;
use gatekeeper_core::gate::vram_rules::layers_overflow;
use gatekeeper_core::params::build_argv;
use gatekeeper_core::params::catalog::ValueKind;
use gatekeeper_core::params::help_parser::parse_help;
use gatekeeper_core::supervisor::log_parse::parse_layer_placement;
use serde_json::json;

const HELP: &str = include_str!("fixtures/llama-server-b11804-help.txt");
const LOG_DEFAULT: &str = include_str!("fixtures/llama-server-b11804-load-default-verbosity.log");
const LOG_FULL: &str = include_str!("fixtures/llama-server-b11804-load-full-offload.log");
const LOG_NGL10: &str = include_str!("fixtures/llama-server-b11804-load-explicit-ngl10.log");
const LOG_FIT_CPU: &str = include_str!("fixtures/llama-server-b11804-load-fit-moved-all-to-cpu.log");

/// First layer placement found anywhere in a log.
fn placement(log: &str) -> Option<(u32, u32)> {
    log.lines().find_map(parse_layer_placement)
}

#[test]
fn layer_placement_is_parsed_from_real_logs() {
    assert_eq!(placement(LOG_FULL), Some((25, 25)));
    assert_eq!(placement(LOG_NGL10), Some((10, 25)));
    assert_eq!(placement(LOG_FIT_CPU), Some((0, 25)));
}

#[test]
fn default_verbosity_logs_have_no_placement_line() {
    // This is why the launcher passes --verbosity 4: without it the
    // overflow check would silently never fire.
    assert_eq!(placement(LOG_DEFAULT), None);
}

#[test]
fn only_the_fit_case_counts_as_overflow_when_ngl_is_automatic() {
    let auto = json!("auto");
    let (o, t) = placement(LOG_FIT_CPU).expect("placement");
    assert!(layers_overflow(o, t, Some(&auto)), "fit moved layers to the CPU: overflow");
    let (o, t) = placement(LOG_FULL).expect("placement");
    assert!(!layers_overflow(o, t, Some(&auto)));
    // 10/25 with -ngl 10 is the user's choice, not a failure (spec §13.2).
    let (o, t) = placement(LOG_NGL10).expect("placement");
    assert!(!layers_overflow(o, t, Some(&json!(10))));
}

#[test]
fn real_help_parses_into_a_rich_catalog() {
    let c = parse_help(HELP, "b11804", "2026-10-21");
    assert!(c.params.len() > 200, "parsed {} parameters", c.params.len());
    assert!(c.params.iter().all(|p| !p.flags.is_empty() && !p.key.is_empty()));

    let ctx = c.resolve("ctx-size").expect("ctx-size");
    assert_eq!(
        (ctx.value_kind, ctx.default.as_deref(), ctx.env.as_deref()),
        (ValueKind::Integer, Some("0"), Some("LLAMA_ARG_CTX_SIZE"))
    );

    // `--gpu-layers` takes a number or the words auto/all, so it must not be a plain integer.
    let ngl = c.resolve("n-gpu-layers").expect("alias n-gpu-layers resolves");
    assert_eq!(ngl.key, "gpu-layers");
    assert_eq!(ngl.value_kind, ValueKind::String);

    let fa = c.resolve("flash-attn").expect("flash-attn");
    assert_eq!(fa.choices.as_deref().map(<[String]>::len), Some(3));

    let fit = c.resolve("fit").expect("fit");
    assert_eq!((fit.value_kind, fit.default.as_deref()), (ValueKind::Enum, Some("on")));

    let ctk = c.resolve("cache-type-k").expect("cache-type-k");
    assert_eq!(ctk.value_kind, ValueKind::Enum);
    assert!(ctk.choices.as_ref().is_some_and(|v| v.iter().any(|x| x == "q8_0")));

    let strict = c.resolve("cpu-strict").expect("cpu-strict");
    assert_eq!((strict.value_kind, strict.choices.as_deref().map(<[String]>::len)), (ValueKind::Enum, Some(2)));
    assert_eq!(c.resolve("poll").expect("poll").value_kind, ValueKind::Integer);

    let jinja = c.resolve("jinja").expect("jinja");
    assert_eq!((jinja.value_kind, jinja.negation_flag.as_deref()), (ValueKind::Toggle, Some("--no-jinja")));
    assert_eq!(c.resolve("temp").expect("temp").value_kind, ValueKind::Float);
    assert_eq!(c.resolve("split-mode").expect("split-mode").choices.as_ref().map(Vec::len), Some(4));

    for reserved in ["model", "port", "host", "api-key"] {
        assert!(
            c.resolve(reserved).unwrap_or_else(|| panic!("{reserved} exists")).reserved,
            "{reserved} must be reserved"
        );
    }
}

#[test]
fn real_catalog_drives_argv_conversion() {
    let c = parse_help(HELP, "b11804", "2026-10-21");
    let mut m = ModelConfig::default();
    m.llama_args.insert("ctx-size".into(), json!(8192));
    m.llama_args.insert("n-gpu-layers".into(), json!("auto"));
    m.llama_args.insert("flash-attn".into(), json!(true));
    m.llama_args.insert("jinja".into(), json!(true));
    m.llama_args.insert("cache-type-k".into(), json!("q8_0"));
    m.llama_args.insert("fit".into(), json!("on"));
    let out = build_argv(&c, &m);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(
        out.args,
        [
            "--cache-type-k",
            "q8_0",
            "--ctx-size",
            "8192",
            "--fit",
            "on",
            "--flash-attn",
            "on",
            "--jinja",
            "--n-gpu-layers",
            "auto"
        ]
    );
}

#[test]
fn reserved_parameters_cannot_be_set_through_a_short_alias() {
    let c = parse_help(HELP, "b11804", "2026-10-21");
    for alias in ["m", "model", "mm", "hf", "hft"] {
        let mut m = ModelConfig::default();
        m.llama_args.insert(alias.into(), json!("evil.gguf"));
        let out = build_argv(&c, &m);
        assert!(out.args.is_empty(), "`{alias}` must not produce arguments: {:?}", out.args);
        assert_eq!(out.warnings.len(), 1, "`{alias}` is reported");
        // Validation (which has no catalog) catches the same aliases.
        let cfg = gatekeeper_core::config::Config { models: vec![m], ..Default::default() };
        let issues =
            gatekeeper_core::config::validate(&cfg, gatekeeper_core::config::ValidateOptions { check_files: false });
        assert!(
            issues.iter().any(|i| i.path.contains(&format!("llama_args.{alias}"))),
            "validation flags `{alias}`: {issues:?}"
        );
    }
}

#[test]
fn verbosity_is_requested_so_the_placement_line_appears() {
    use gatekeeper_core::supervisor::launcher::build_args;
    let c = parse_help(HELP, "b11804", "2026-10-21");
    let mut m = ModelConfig::default();
    m.files.model = "m.gguf".into();
    let cfg = gatekeeper_core::config::Config::default();
    let args = build_args(&c, &cfg, &m, 1, "k");
    assert!(args.windows(2).any(|w| w == ["--verbosity", "4"]), "{args:?}");
    // An explicit user choice is respected.
    m.llama_args.insert("verbosity".into(), json!(3));
    let args = build_args(&c, &cfg, &m, 1, "k");
    assert_eq!(args.iter().filter(|a| a.as_str() == "--verbosity").count(), 1);
    assert!(args.windows(2).any(|w| w == ["--verbosity", "3"]));
}

#[test]
fn legacy_draft_key_in_llama_args_is_not_emitted() {
    let c = parse_help(HELP, "b11804", "2026-10-27");
    for key in ["spec-draft-model", "md", "model-draft"] {
        let mut m = ModelConfig::default();
        m.llama_args.insert(key.to_owned(), json!("d.gguf"));
        let out = build_argv(&c, &m);
        assert!(!out.args.iter().any(|a| a == "d.gguf"), "{key} must not reach the engine: {:?}", out.args);
        assert_eq!(out.warnings.len(), 1, "{key} is listed as skipped");
    }
}
