//! Speculative decoding types and launch arguments (spec §10.6).
//!
//! Responsibility: the vocabulary of `--spec-type`, reading the selected types
//! out of `llama_args`, recognising what a draft file is, and emitting the
//! draft-file argument only when the engine will use it.
//! Not responsible for: findings (`findings`) or memory estimates
//! (`supervisor::footprint`).

use std::collections::BTreeMap;

use serde_json::Value;

use crate::config::schema::ModelConfig;
use crate::gguf::header::GgufMeta;
use crate::params::catalog::ParamCatalog;
use crate::params::{ArgvOutput, ArgvWarning};

/// The `llama_args` key that holds the comma-separated list of types.
const SPEC_TYPE_KEY: &str = "spec-type";

/// The parameter the draft file is passed with; the catalog knows its aliases.
const DRAFT_MODEL_PARAM: &str = "spec-draft-model";

/// One speculative decoding method the engine can be asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecType {
    /// A smaller model with the same vocabulary drafts the tokens.
    DraftSimple,
    /// One-layer draft that reads the target's hidden states.
    DraftEagle3,
    /// Block-diffusion draft.
    DraftDflash,
    /// DFlash plus a Markov head.
    DraftDspark,
    /// Multi-token prediction layers.
    DraftMtp,
    /// N-gram statistics, simple form.
    NgramSimple,
    /// N-gram map keyed by n-grams.
    NgramMapK,
    /// N-gram map with keys and values.
    NgramMapK4v,
    /// N-gram module.
    NgramMod,
    /// N-gram cache.
    NgramCache,
    /// A type this app has no name for (a newer engine).
    Other(String),
}

impl SpecType {
    /// The engine's identifier, as written in `--spec-type`.
    pub fn id(&self) -> String {
        match self {
            Self::DraftSimple => "draft-simple",
            Self::DraftEagle3 => "draft-eagle3",
            Self::DraftDflash => "draft-dflash",
            Self::DraftDspark => "draft-dspark",
            Self::DraftMtp => "draft-mtp",
            Self::NgramSimple => "ngram-simple",
            Self::NgramMapK => "ngram-map-k",
            Self::NgramMapK4v => "ngram-map-k4v",
            Self::NgramMod => "ngram-mod",
            Self::NgramCache => "ngram-cache",
            Self::Other(id) => id,
        }
        .to_owned()
    }

    /// Parses an identifier (case-insensitive, surrounding space ignored).
    pub fn from_id(text: &str) -> Self {
        let id = text.trim().to_ascii_lowercase();
        match id.as_str() {
            "draft-simple" => Self::DraftSimple,
            "draft-eagle3" => Self::DraftEagle3,
            "draft-dflash" => Self::DraftDflash,
            "draft-dspark" => Self::DraftDspark,
            "draft-mtp" => Self::DraftMtp,
            "ngram-simple" => Self::NgramSimple,
            "ngram-map-k" => Self::NgramMapK,
            "ngram-map-k4v" => Self::NgramMapK4v,
            "ngram-mod" => Self::NgramMod,
            "ngram-cache" => Self::NgramCache,
            _ => Self::Other(id),
        }
    }

    /// True for types that cannot work without a draft file.
    pub fn needs_file(&self) -> bool {
        matches!(self, Self::DraftSimple | Self::DraftEagle3 | Self::DraftDflash | Self::DraftDspark)
    }

    /// True for every `draft-*` type, including MTP, which may take a head file.
    pub fn is_draft(&self) -> bool {
        self.id().starts_with("draft-")
    }

    /// True for types that draft without a model (`ngram-*`).
    pub fn is_draftless(&self) -> bool {
        self.id().starts_with("ngram-")
    }
}

/// The types named in `spec-type`, in order, without duplicates. Absent, empty
/// and `none` all mean Off. Tolerates spaces, upper case and empty entries,
/// because the value may have been written by hand.
pub fn selected_types(llama_args: &BTreeMap<String, Value>) -> Vec<SpecType> {
    let text = match llama_args.get(SPEC_TYPE_KEY) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","),
        _ => return Vec::new(),
    };
    let mut out: Vec<SpecType> = Vec::new();
    for part in text.split(',') {
        let id = part.trim();
        if id.is_empty() || id.eq_ignore_ascii_case("none") {
            continue;
        }
        let ty = SpecType::from_id(id);
        if !out.contains(&ty) {
            out.push(ty);
        }
    }
    out
}

/// The `spec-type` value for `types`, or `None` for Off (the key is then removed).
pub fn join_types(types: &[SpecType]) -> Option<String> {
    (!types.is_empty()).then(|| types.iter().map(SpecType::id).collect::<Vec<_>>().join(","))
}

/// `llama_args` with `ty` added to the selected types (kept if already there).
pub fn with_type_added(llama_args: &mut BTreeMap<String, Value>, ty: SpecType) {
    let mut types = selected_types(llama_args);
    if !types.contains(&ty) {
        types.push(ty);
    }
    match join_types(&types) {
        Some(value) => {
            llama_args.insert(SPEC_TYPE_KEY.to_owned(), Value::String(value));
        }
        None => {
            llama_args.remove(SPEC_TYPE_KEY);
        }
    }
}

/// Makes `path` the model's draft file (spec §10.6 "Add as a Draft Model"). If
/// no draft type is selected yet, adds the one the file appears to be (from its
/// content, then its name), or a separate draft model when nothing is
/// recognised; otherwise the user's choice is left alone.
pub fn attach_draft(model: &mut ModelConfig, path: &str) {
    model.files.draft = Some(path.to_owned());
    if selected_types(&model.llama_args).iter().any(SpecType::is_draft) {
        return;
    }
    let ty = crate::gguf::header::read_meta_cached(std::path::Path::new(path))
        .ok()
        .and_then(|meta| kind_from_meta(&meta))
        .or_else(|| kind_from_file_name(path))
        .unwrap_or(SpecType::DraftSimple);
    with_type_added(&mut model.llama_args, ty);
}

/// The types the installed engine lists in its help, or `None` when it has no
/// `--spec-type`. The help parser reports the entry as a plain string, so the
/// placeholder (`none,draft-simple,…`) is split here.
pub fn available_types(catalog: &ParamCatalog) -> Option<Vec<String>> {
    let placeholder = catalog.resolve(SPEC_TYPE_KEY)?.placeholder.as_deref()?;
    Some(
        placeholder
            .split(',')
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty() && t != "none")
            .collect(),
    )
}

/// What kind of draft a file appears to be, by the rules the engine itself
/// uses to infer a type: architecture `dflash` is DFlash (DSpark with a
/// `markov_w1.weight` tensor); a `blk.<last>.nextn.eh_proj.weight` tensor is
/// MTP. A plain model and an EAGLE-3 draft are not recognisable.
pub fn kind_from_meta(meta: &GgufMeta) -> Option<SpecType> {
    if meta.architecture.as_deref() == Some("dflash") {
        return Some(if meta.has_tensor("markov_w1.weight") { SpecType::DraftDspark } else { SpecType::DraftDflash });
    }
    let last = meta.block_count?.checked_sub(1)?;
    meta.has_tensor(&format!("blk.{last}.nextn.eh_proj.weight")).then_some(SpecType::DraftMtp)
}

/// What a downloaded file is called after, using the prefixes the engine's own
/// downloader looks for (`mtp-`, `dflash-`, `dspark-`) and the word `eagle3`.
pub fn kind_from_file_name(name: &str) -> Option<SpecType> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase();
    if base.starts_with("mtp-") {
        Some(SpecType::DraftMtp)
    } else if base.starts_with("dflash-") {
        Some(SpecType::DraftDflash)
    } else if base.starts_with("dspark-") {
        Some(SpecType::DraftDspark)
    } else if base.contains("eagle3") {
        Some(SpecType::DraftEagle3)
    } else {
        None
    }
}

/// True if the main model itself contains multi-token-prediction layers.
pub fn has_mtp_layers(meta: &GgufMeta) -> bool {
    meta.nextn_predict_layers.is_some_and(|n| n > 0) || kind_from_meta(meta) == Some(SpecType::DraftMtp)
}

/// True if the draft file will be handed to the engine: a file is set and at
/// least one `draft-*` type is selected. The app never relies on the engine
/// inferring the type from the file (spec §10.6).
pub fn draft_is_passed(model: &ModelConfig) -> bool {
    model.files.draft.as_deref().is_some_and(|d| !d.trim().is_empty())
        && selected_types(&model.llama_args).iter().any(SpecType::is_draft)
}

/// The launch arguments for the draft file: `[flag, path]` when it is passed.
/// The flag is the first long spelling the installed engine lists; an engine
/// that lists none skips the file with a warning.
pub fn launch_args(catalog: &ParamCatalog, model: &ModelConfig) -> ArgvOutput {
    let mut out = ArgvOutput::default();
    if !draft_is_passed(model) {
        return out;
    }
    let path = model.files.draft.clone().unwrap_or_default();
    let flag = catalog.resolve(DRAFT_MODEL_PARAM).and_then(|p| p.flags.iter().find(|f| f.starts_with("--")));
    match flag {
        Some(flag) => out.args.extend([flag.clone(), path]),
        None => out.warnings.push(ArgvWarning {
            key: "files.draft".to_owned(),
            message: "the installed engine has no draft model option, so the draft file is not used".to_owned(),
        }),
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::gguf::header::read_meta_from;
    use crate::gguf::header::tests::{extend_gguf, sample_gguf};
    use crate::params::help_parser::parse_help;

    const HELP: &str = include_str!("../../tests/fixtures/llama-server-b11804-help.txt");

    fn catalog() -> ParamCatalog {
        parse_help(HELP, "b11804", "2026-10-27")
    }

    fn model(spec_type: Option<&str>, draft: Option<&str>) -> ModelConfig {
        let mut m = ModelConfig::default();
        m.files.model = "m.gguf".into();
        m.files.draft = draft.map(str::to_owned);
        if let Some(t) = spec_type {
            m.llama_args.insert("spec-type".into(), json!(t));
        }
        m
    }

    fn meta(arch: &str, layers: u32, tensors: &[&str]) -> GgufMeta {
        let bytes = extend_gguf(sample_gguf(arch, layers, 100, 64, 4, 4, 1), &[], &[], tensors);
        read_meta_from(&mut std::io::Cursor::new(bytes)).expect("parse")
    }

    #[test]
    fn spec_type_parsing_is_tolerant() {
        let m = model(Some("Draft-Simple, ngram-mod,,none"), None);
        assert_eq!(selected_types(&m.llama_args), [SpecType::DraftSimple, SpecType::NgramMod]);
        assert!(selected_types(&model(None, None).llama_args).is_empty());
        assert!(selected_types(&model(Some("none"), None).llama_args).is_empty());
        assert_eq!(selected_types(&model(Some("ngram-mod,NGRAM-MOD"), None).llama_args), [SpecType::NgramMod]);
        assert_eq!(
            selected_types(&model(Some("draft-future"), None).llama_args),
            [SpecType::Other("draft-future".into())]
        );
    }

    #[test]
    fn joining_and_adding_types_round_trips() {
        assert_eq!(join_types(&[]), None);
        assert_eq!(join_types(&[SpecType::DraftMtp, SpecType::NgramMod]).as_deref(), Some("draft-mtp,ngram-mod"));
        let mut m = model(Some("ngram-mod"), None);
        with_type_added(&mut m.llama_args, SpecType::DraftSimple);
        assert_eq!(m.llama_args["spec-type"], "ngram-mod,draft-simple");
        with_type_added(&mut m.llama_args, SpecType::DraftSimple);
        assert_eq!(m.llama_args["spec-type"], "ngram-mod,draft-simple", "no duplicate");
    }

    #[test]
    fn type_classes_follow_the_spec_table() {
        assert!(SpecType::DraftSimple.needs_file() && SpecType::DraftDspark.needs_file());
        assert!(!SpecType::DraftMtp.needs_file() && SpecType::DraftMtp.is_draft());
        assert!(SpecType::NgramCache.is_draftless() && !SpecType::NgramCache.is_draft());
    }

    #[test]
    fn available_types_come_from_the_help_placeholder() {
        let types = available_types(&catalog()).expect("b11804 has --spec-type");
        assert!(types.contains(&"draft-mtp".to_owned()) && types.contains(&"ngram-cache".to_owned()));
        assert!(!types.contains(&"none".to_owned()));
        assert_eq!(available_types(&ParamCatalog::default()), None);
    }

    #[test]
    fn kind_detection_follows_the_engine_rules() {
        assert_eq!(kind_from_meta(&meta("dflash", 4, &[])), Some(SpecType::DraftDflash));
        assert_eq!(kind_from_meta(&meta("dflash", 4, &["markov_w1.weight"])), Some(SpecType::DraftDspark));
        assert_eq!(kind_from_meta(&meta("qwen3", 36, &["blk.35.nextn.eh_proj.weight"])), Some(SpecType::DraftMtp));
        assert_eq!(
            kind_from_meta(&meta("qwen3", 36, &["blk.34.nextn.eh_proj.weight"])),
            None,
            "only the last layer counts"
        );
        assert_eq!(kind_from_meta(&meta("qwen3", 36, &[])), None);
    }

    #[test]
    fn mtp_layers_are_found_by_metadata_or_tensor() {
        assert!(has_mtp_layers(&meta("qwen3", 36, &["blk.35.nextn.eh_proj.weight"])));
        let by_key =
            extend_gguf(sample_gguf("qwen3", 36, 1, 1, 1, 1, 0), &[("qwen3.nextn_predict_layers", 1)], &[], &[]);
        assert!(has_mtp_layers(&read_meta_from(&mut std::io::Cursor::new(by_key)).expect("parse")));
        assert!(!has_mtp_layers(&meta("qwen3", 36, &[])));
    }

    #[test]
    fn attaching_a_draft_selects_a_type_only_when_none_is_checked() {
        let dir = tempfile::tempdir().expect("dir");
        let plain = dir.path().join("Qwen3-0.6B-Q8_0.gguf");
        std::fs::write(&plain, sample_gguf("qwen3", 28, 100, 64, 4, 4, 7)).expect("write");
        let plain = plain.to_string_lossy().into_owned();
        let head = dir.path().join("head.gguf");
        std::fs::write(
            &head,
            extend_gguf(sample_gguf("qwen3", 36, 100, 64, 4, 4, 7), &[], &[], &["blk.35.nextn.eh_proj.weight"]),
        )
        .expect("write");
        let head = head.to_string_lossy().into_owned();

        let mut m = model(None, None);
        attach_draft(&mut m, &plain);
        assert_eq!(m.files.draft.as_deref(), Some(plain.as_str()));
        assert_eq!(m.llama_args["spec-type"], "draft-simple", "nothing recognised: a separate draft model");

        let mut mtp = model(None, None);
        attach_draft(&mut mtp, &head);
        assert_eq!(mtp.llama_args["spec-type"], "draft-mtp", "detected from the tensors");

        let mut chosen = model(Some("draft-eagle3"), Some("old.gguf"));
        attach_draft(&mut chosen, &plain);
        assert_eq!(chosen.llama_args["spec-type"], "draft-eagle3", "the user's choice stands");
        assert_eq!(chosen.files.draft.as_deref(), Some(plain.as_str()), "the old file is replaced");
    }

    #[test]
    fn attaching_a_draft_keeps_ngram_choices() {
        let mut m = model(Some("ngram-mod"), None);
        attach_draft(&mut m, "mtp-x.gguf");
        assert_eq!(m.llama_args["spec-type"], "ngram-mod,draft-mtp", "falls back to the file name when unreadable");
    }

    #[test]
    fn file_names_map_to_types() {
        assert_eq!(kind_from_file_name("sub/MTP-Qwen3-Q4_K_M.gguf"), Some(SpecType::DraftMtp));
        assert_eq!(kind_from_file_name("dflash-x.gguf"), Some(SpecType::DraftDflash));
        assert_eq!(kind_from_file_name("dspark-x.gguf"), Some(SpecType::DraftDspark));
        assert_eq!(kind_from_file_name("Qwen3-8B-EAGLE3-f16.gguf"), Some(SpecType::DraftEagle3));
        assert_eq!(kind_from_file_name("Qwen3-0.6B-Q8_0.gguf"), None);
    }

    #[test]
    fn draft_file_is_passed_with_a_draft_type() {
        let out = launch_args(&catalog(), &model(Some("draft-simple"), Some("C:\\m\\draft model.gguf")));
        assert_eq!(out.args, ["--spec-draft-model", "C:\\m\\draft model.gguf"], "one argv element, spaces and all");
        assert!(out.warnings.is_empty());
        let mtp = launch_args(&catalog(), &model(Some("draft-mtp"), Some("mtp-head.gguf")));
        assert_eq!(mtp.args.len(), 2, "an MTP head file is passed too");
    }

    #[test]
    fn draft_file_is_not_passed_without_a_draft_type() {
        for spec in [None, Some("none"), Some("ngram-mod"), Some("ngram-simple,ngram-cache")] {
            let out = launch_args(&catalog(), &model(spec, Some("d.gguf")));
            assert!(out.args.is_empty() && out.warnings.is_empty(), "{spec:?}");
        }
    }

    #[test]
    fn empty_draft_path_is_not_passed() {
        assert!(launch_args(&catalog(), &model(Some("draft-mtp"), Some("  "))).args.is_empty());
        assert!(launch_args(&catalog(), &model(Some("draft-mtp"), None)).args.is_empty());
    }

    #[test]
    fn missing_catalog_entry_skips_the_file_with_a_warning() {
        let out = launch_args(&ParamCatalog::default(), &model(Some("draft-simple"), Some("d.gguf")));
        assert!(out.args.is_empty());
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].key, "files.draft");
    }
}
