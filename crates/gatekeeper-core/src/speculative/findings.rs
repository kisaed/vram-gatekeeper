//! Findings about a model's speculative setup, and the file summary the
//! Speculative Decoding tab shows (spec §10.6).
//!
//! Responsibility: apply the validation table of §10.6 to one model, so a
//! setup that is likely to fail is reported before the model is loaded.
//! Not responsible for: the validation framework (`config::validate`), which
//! calls [`findings`] for every model.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::Serialize;

use super::types::{SpecType, available_types, has_mtp_layers, kind_from_meta, selected_types};
use crate::config::schema::ModelConfig;
use crate::config::validate::ValidationIssue;
use crate::error::CoreResult;
use crate::gguf::header::{GgufMeta, read_meta_cached};
use crate::params::catalog::ParamCatalog;

/// Largest vocabulary size difference the engine accepts between a target and
/// a draft model (`SPEC_VOCAB_MAX_SIZE_DIFFERENCE` in llama.cpp).
const VOCAB_MAX_DIFFERENCE: u64 = 128;

/// `llama_args` spellings of the draft file that older configs may hold.
const LEGACY_DRAFT_KEYS: [&str; 3] = ["spec-draft-model", "md", "model-draft"];

/// Whether the engine reported starting one configured type (Test Load, spec §10.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpecConfirmation {
    /// The type's identifier, e.g. `draft-simple`.
    pub spec_type: String,
    /// True when the engine's log named the type. False means "not confirmed",
    /// not "off": a lowered log verbosity hides the line too.
    pub confirmed: bool,
}

/// One entry per `selected` type, confirmed when `started` names it.
pub fn confirmations(selected: &[SpecType], started: &[String]) -> Vec<SpecConfirmation> {
    selected
        .iter()
        .map(|ty| SpecConfirmation {
            spec_type: ty.id(),
            confirmed: started.iter().any(|s| s.eq_ignore_ascii_case(&ty.id())),
        })
        .collect()
}

/// What the Speculative Decoding tab shows about a GGUF file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GgufSummary {
    /// `general.architecture`.
    pub architecture: Option<String>,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Number of layers.
    pub block_count: Option<u32>,
    /// Number of vocabulary entries.
    pub vocab_size: Option<u64>,
    /// `tokenizer.ggml.model`.
    pub tokenizer_model: Option<String>,
    /// Multi-token-prediction layers declared by the file.
    pub nextn_predict_layers: Option<u32>,
    /// True if the file contains multi-token-prediction layers.
    pub has_mtp_layers: bool,
    /// The speculative type the engine would infer from the file (`draft-mtp`,
    /// `draft-dflash`, `draft-dspark`), if any.
    pub detected_kind: Option<String>,
}

/// Reads the summary of `path`.
///
/// # Errors
/// Returns an error if the file cannot be read or is not a GGUF file.
pub fn summarize(path: &Path) -> CoreResult<GgufSummary> {
    let size_bytes = std::fs::metadata(path).map_err(|e| crate::error::CoreError::io(path, e))?.len();
    let meta = read_meta_cached(path)?;
    Ok(GgufSummary {
        architecture: meta.architecture.clone(),
        size_bytes,
        block_count: meta.block_count,
        vocab_size: meta.vocab_size,
        tokenizer_model: meta.tokenizer_model.clone(),
        nextn_predict_layers: meta.nextn_predict_layers,
        has_mtp_layers: has_mtp_layers(&meta),
        detected_kind: kind_from_meta(&meta).map(|k| k.id()),
    })
}

/// True if the file starts with the GGUF signature.
fn has_gguf_signature(path: &Path) -> bool {
    let mut magic = [0u8; 4];
    File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"GGUF"
}

/// True if `path` names a file that can be inspected.
fn is_file(path: &str) -> bool {
    Path::new(path).is_file()
}

/// Applies the validation table of spec §10.6 to `model`. `base` is the
/// model's JSON path (`models[2]`). Rules that read files run only when
/// `check_files` is set; rules about the installed engine run only when a
/// `catalog` is given.
pub fn findings(
    model: &ModelConfig,
    base: &str,
    catalog: Option<&ParamCatalog>,
    check_files: bool,
) -> Vec<ValidationIssue> {
    let mut out = Vec::new();
    let types = selected_types(&model.llama_args);
    let draft = model.files.draft.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let type_path = format!("{base}.llama_args.spec-type");
    let draft_path = format!("{base}.files.draft");

    let any_needs_file = types.iter().any(SpecType::needs_file);
    let any_draft = types.iter().any(SpecType::is_draft);
    let any_draftless = types.iter().any(SpecType::is_draftless);

    if any_needs_file && draft.is_none() {
        let legacy = LEGACY_DRAFT_KEYS.iter().any(|k| model.llama_args.contains_key(*k));
        out.push(if legacy {
            // The old Parameters entry is no longer passed, but it must not make the file unusable.
            ValidationIssue::warning(
                &type_path,
                "This type needs a draft file. The draft model set on the Parameters tab is no longer passed to the engine; use Move to Draft File on the Speculative Decoding tab.",
            )
        } else {
            ValidationIssue::new(&type_path, "This type needs a draft file.")
        });
    }
    if draft.is_some() && !any_draft {
        out.push(ValidationIssue::warning(
            &draft_path,
            "The draft file is kept but not used, because no draft type is selected.",
        ));
    }
    if any_draft && any_draftless {
        out.push(ValidationIssue::warning(
            &type_path,
            "An N-Gram type is selected together with a draft type; the engine gives the N-Gram type precedence.",
        ));
    }
    if types.iter().any(|t| matches!(t, SpecType::DraftEagle3 | SpecType::DraftDflash | SpecType::DraftDspark)) {
        out.push(ValidationIssue::info(
            &type_path,
            "This draft must be converted for this exact model, which the application cannot check.",
        ));
    }
    if let Some(catalog) = catalog {
        let offered = available_types(catalog);
        for ty in &types {
            let listed = offered.as_ref().is_some_and(|o| o.contains(&ty.id()));
            if !listed {
                out.push(ValidationIssue::warning(
                    &type_path,
                    format!("The installed engine does not support {}; it is skipped.", ty.id()),
                ));
            }
        }
    }

    // Models that use no speculative decoding are not opened at all.
    if check_files && (!types.is_empty() || draft.is_some()) {
        file_findings(model, draft, &types, &type_path, &draft_path, model.enabled, &mut out);
    }
    out
}

/// The rules that read GGUF files.
fn file_findings(
    model: &ModelConfig,
    draft: Option<&str>,
    types: &[SpecType],
    type_path: &str,
    draft_path: &str,
    enabled: bool,
    out: &mut Vec<ValidationIssue>,
) {
    let main_meta = (!model.files.model.trim().is_empty() && is_file(&model.files.model))
        .then(|| read_meta_cached(Path::new(&model.files.model)).ok())
        .flatten();

    let mut draft_meta: Option<std::sync::Arc<GgufMeta>> = None;
    if let Some(d) = draft
        && enabled
    {
        let path = Path::new(d);
        if !path.is_file() || !has_gguf_signature(path) {
            out.push(ValidationIssue::new(draft_path, "The draft file is missing or is not a GGUF file."));
        } else {
            draft_meta = read_meta_cached(path).ok();
        }
    }

    if types.contains(&SpecType::DraftMtp)
        && draft.is_none()
        && let Some(main) = &main_meta
        && !has_mtp_layers(main)
    {
        out.push(ValidationIssue::warning(
            type_path,
            "The model may not contain multi-token prediction layers; select an MTP head file or a model that includes them.",
        ));
    }

    let Some(draft_meta) = draft_meta else { return };
    let draft_types: Vec<&SpecType> = types.iter().filter(|t| t.is_draft()).collect();
    if let Some(kind) = kind_from_meta(&draft_meta)
        && !draft_types.is_empty()
        && !draft_types.contains(&&kind)
    {
        out.push(ValidationIssue::warning(
            draft_path,
            format!("The draft file appears to be a different kind of draft ({}).", kind.id()),
        ));
    }
    // The engine compares vocabularies for a separate draft model only.
    if types.contains(&SpecType::DraftSimple)
        && let Some(main) = &main_meta
        && vocabularies_differ(main, &draft_meta)
    {
        out.push(ValidationIssue::warning(
            draft_path,
            "The draft and target vocabularies differ, so the engine will probably refuse this draft file and the model will fail to load.",
        ));
    }
}

/// True if the tokenizer type differs, or the sizes differ by more than the
/// engine tolerates. A value missing from either file is not a difference.
fn vocabularies_differ(target: &GgufMeta, draft: &GgufMeta) -> bool {
    let model_differs = matches!((&target.tokenizer_model, &draft.tokenizer_model), (Some(a), Some(b)) if a != b);
    let size_differs =
        matches!((target.vocab_size, draft.vocab_size), (Some(a), Some(b)) if a.abs_diff(b) > VOCAB_MAX_DIFFERENCE);
    model_differs || size_differs
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::config::validate::Severity;
    use crate::gguf::header::tests::{extend_gguf, sample_gguf_vocab};
    use crate::params::help_parser::parse_help;

    const HELP: &str = include_str!("../../tests/fixtures/llama-server-b11804-help.txt");

    /// A temporary folder with GGUF files written into it.
    struct Files(tempfile::TempDir);

    impl Files {
        fn new() -> Self {
            Self(tempfile::tempdir().expect("dir"))
        }

        fn gguf(&self, name: &str, vocab: u64, tokenizer: &str, uints: &[(&str, u32)], tensors: &[&str]) -> String {
            let bytes = extend_gguf(
                sample_gguf_vocab("qwen3", 36, 8192, 64, 4, 4, 15, vocab),
                uints,
                &[("tokenizer.ggml.model", tokenizer)],
                tensors,
            );
            let path: PathBuf = self.0.path().join(name);
            std::fs::write(&path, bytes).expect("write");
            path.to_string_lossy().into_owned()
        }
    }

    fn model(main: &str, draft: Option<&str>, spec: Option<&str>) -> ModelConfig {
        let mut m = ModelConfig { id: "m".into(), ..ModelConfig::default() };
        m.files.model = main.into();
        m.files.draft = draft.map(str::to_owned);
        if let Some(s) = spec {
            m.llama_args.insert("spec-type".into(), json!(s));
        }
        m
    }

    fn run(m: &ModelConfig) -> Vec<ValidationIssue> {
        findings(m, "models[0]", None, true)
    }

    fn has(found: &[ValidationIssue], severity: Severity, path: &str, text: &str) -> bool {
        found.iter().any(|i| i.severity == severity && i.path == path && i.message.contains(text))
    }

    #[test]
    fn needs_file_type_without_draft_is_an_error() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let found = run(&model(&main, None, Some("draft-simple")));
        assert!(has(&found, Severity::Error, "models[0].llama_args.spec-type", "needs a draft file"), "{found:?}");
        assert!(run(&model(&main, None, Some("ngram-mod"))).is_empty());
    }

    #[test]
    fn legacy_draft_key_with_a_file_type_and_no_file_is_a_warning_not_an_error() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let mut m = model(&main, None, Some("draft-simple"));
        m.llama_args.insert("spec-draft-model".into(), json!("old.gguf"));
        let found = run(&m);
        assert!(has(&found, Severity::Warning, "models[0].llama_args.spec-type", "Move to Draft File"), "{found:?}");
        assert!(found.iter().all(|i| i.severity != Severity::Error));
    }

    #[test]
    fn draft_missing_or_not_gguf_is_an_error() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let missing = run(&model(&main, Some("nowhere.gguf"), Some("draft-simple")));
        assert!(has(&missing, Severity::Error, "models[0].files.draft", "missing or is not a GGUF"), "{missing:?}");
        let text = f.0.path().join("notes.gguf");
        std::fs::write(&text, "not a model").expect("write");
        let fake = run(&model(&main, text.to_str(), Some("draft-simple")));
        assert!(has(&fake, Severity::Error, "models[0].files.draft", "missing or is not a GGUF"), "{fake:?}");
        // Disabled models are not checked against the disk, like the model file itself.
        let mut off = model(&main, Some("nowhere.gguf"), Some("draft-simple"));
        off.enabled = false;
        assert!(run(&off).iter().all(|i| i.path != "models[0].files.draft"));
        // And nothing touches the disk when file checks are off.
        assert!(
            findings(&model(&main, Some("nowhere.gguf"), Some("draft-simple")), "models[0]", None, false).is_empty()
        );
    }

    #[test]
    fn unused_draft_is_a_warning() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let draft = f.gguf("draft.gguf", 100, "gpt2", &[], &[]);
        for spec in [None, Some("ngram-mod")] {
            let found = run(&model(&main, Some(&draft), spec));
            assert!(has(&found, Severity::Warning, "models[0].files.draft", "kept but not used"), "{spec:?} {found:?}");
        }
        assert!(run(&model(&main, Some(&draft), Some("draft-simple"))).is_empty());
    }

    #[test]
    fn draft_type_with_ngram_warns_about_precedence() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let draft = f.gguf("draft.gguf", 100, "gpt2", &[], &[]);
        let found = run(&model(&main, Some(&draft), Some("draft-simple,ngram-mod")));
        assert!(has(&found, Severity::Warning, "models[0].llama_args.spec-type", "precedence"), "{found:?}");
    }

    #[test]
    fn mtp_without_layers_or_file_warns() {
        let f = Files::new();
        let plain = f.gguf("plain.gguf", 100, "gpt2", &[], &[]);
        let found = run(&model(&plain, None, Some("draft-mtp")));
        assert!(
            has(&found, Severity::Warning, "models[0].llama_args.spec-type", "multi-token prediction"),
            "{found:?}"
        );
        let with_layers = f.gguf("mtp.gguf", 100, "gpt2", &[("qwen3.nextn_predict_layers", 1)], &[]);
        assert!(run(&model(&with_layers, None, Some("draft-mtp"))).is_empty());
        let with_tensor = f.gguf("mtp2.gguf", 100, "gpt2", &[], &["blk.35.nextn.eh_proj.weight"]);
        assert!(run(&model(&with_tensor, None, Some("draft-mtp"))).is_empty());
        let head = f.gguf("head.gguf", 100, "gpt2", &[], &["blk.35.nextn.eh_proj.weight"]);
        assert!(run(&model(&plain, Some(&head), Some("draft-mtp"))).is_empty(), "a head file replaces the layers");
    }

    #[test]
    fn detected_kind_contradicting_type_warns() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 100, "gpt2", &[], &[]);
        let head = f.gguf("head.gguf", 100, "gpt2", &[], &["blk.35.nextn.eh_proj.weight"]);
        let found = run(&model(&main, Some(&head), Some("draft-simple")));
        assert!(has(&found, Severity::Warning, "models[0].files.draft", "different kind of draft"), "{found:?}");
        assert!(run(&model(&main, Some(&head), Some("draft-mtp"))).is_empty());
    }

    #[test]
    fn vocabulary_mismatch_warns() {
        let f = Files::new();
        let main = f.gguf("main.gguf", 1000, "gpt2", &[], &[]);
        let ok = f.gguf("ok.gguf", 1128, "gpt2", &[], &[]);
        let too_far = f.gguf("far.gguf", 1129, "gpt2", &[], &[]);
        let other_tokenizer = f.gguf("spm.gguf", 1000, "llama", &[], &[]);
        assert!(run(&model(&main, Some(&ok), Some("draft-simple"))).is_empty(), "a difference of 128 is accepted");
        for bad in [&too_far, &other_tokenizer] {
            let found = run(&model(&main, Some(bad), Some("draft-simple")));
            assert!(has(&found, Severity::Warning, "models[0].files.draft", "vocabularies differ"), "{found:?}");
        }
        // A converted draft (EAGLE-3 and relatives) legitimately has its own vocabulary.
        assert!(
            run(&model(&main, Some(&too_far), Some("draft-eagle3")))
                .iter()
                .all(|i| !i.message.contains("vocabularies"))
        );
    }

    #[test]
    fn unsupported_type_warns_only_with_a_catalog() {
        let catalog = parse_help(HELP, "b11804", "2026-10-27");
        let m = model("m.gguf", None, Some("ngram-future,ngram-mod"));
        assert!(findings(&m, "models[0]", None, false).is_empty());
        let found = findings(&m, "models[0]", Some(&catalog), false);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(has(&found, Severity::Warning, "models[0].llama_args.spec-type", "ngram-future"));
        let no_support = findings(&m, "models[0]", Some(&ParamCatalog::default()), false);
        assert_eq!(no_support.len(), 2, "an engine without --spec-type supports neither");
    }

    #[test]
    fn eagle3_dflash_dspark_get_an_information_note() {
        for ty in ["draft-eagle3", "draft-dflash", "draft-dspark"] {
            let found = findings(&model("m.gguf", Some("d.gguf"), Some(ty)), "models[0]", None, false);
            assert!(
                has(&found, Severity::Info, "models[0].llama_args.spec-type", "converted for this exact model"),
                "{ty}"
            );
        }
        assert!(findings(&model("m.gguf", Some("d.gguf"), Some("draft-simple")), "models[0]", None, false).is_empty());
    }

    #[test]
    fn confirmations_mark_each_configured_type() {
        let selected = [SpecType::DraftSimple, SpecType::NgramMod];
        let got = confirmations(&selected, &["draft-simple".to_owned()]);
        assert_eq!(
            got,
            [
                SpecConfirmation { spec_type: "draft-simple".into(), confirmed: true },
                SpecConfirmation { spec_type: "ngram-mod".into(), confirmed: false },
            ]
        );
        assert!(confirmations(&[], &["draft-simple".to_owned()]).is_empty(), "nothing configured, nothing reported");
    }

    #[test]
    fn summary_reports_the_fields_the_tab_shows() {
        let f = Files::new();
        let head =
            f.gguf("head.gguf", 321, "gpt2", &[("qwen3.nextn_predict_layers", 1)], &["blk.35.nextn.eh_proj.weight"]);
        let s = summarize(Path::new(&head)).expect("summary");
        assert_eq!((s.architecture.as_deref(), s.block_count, s.vocab_size), (Some("qwen3"), Some(36), Some(321)));
        assert_eq!(s.nextn_predict_layers, Some(1));
        assert!(s.has_mtp_layers);
        assert_eq!(s.detected_kind.as_deref(), Some("draft-mtp"));
        assert!(s.size_bytes > 0);
        assert!(summarize(Path::new("nowhere.gguf")).is_err());
    }
}
