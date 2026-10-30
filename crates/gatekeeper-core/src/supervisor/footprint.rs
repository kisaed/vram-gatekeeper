//! VRAM footprint keys and estimates (spec §9.5).
//!
//! Responsibility: identify which settings change a model's memory use (so a
//! measurement is only reused when still valid), and estimate the footprint
//! before any measurement exists.
//! Not responsible for: measuring (the controller reads per-process
//! counters) or persisting (see `ports::StateStore`).

use sha2::{Digest, Sha256};

use crate::config::schema::ModelConfig;
use crate::gate::vram_rules::estimate_footprint_mib;
use crate::gguf::header::{kv_cache_bytes, read_meta_cached};
use crate::gpu::BYTES_PER_MIB;
use crate::speculative::{SpecType, draft_is_passed, selected_types};

/// Context size assumed when neither `ctx-size` nor the model says.
const FALLBACK_CTX: u32 = 8192;

/// `llama_args` keys that change memory use. A different value means a new
/// measurement (spec §9.5, §10.6). Aliases of the same flag are listed together.
const MEMORY_KEYS: &[&str] = &[
    "ctx-size",
    "n-gpu-layers",
    "gpu-layers",
    "ngl",
    "cache-type-k",
    "cache-type-v",
    "batch-size",
    "ubatch-size",
    "parallel",
    "flash-attn",
    "kv-offload",
    "mmproj",
    "no-kv-offload",
    "tensor-split",
    "split-mode",
    "override-tensor",
    // Speculative decoding (spec §10.6): the draft model's memory.
    "spec-type",
    "spec-draft-ngl",
    "ngld",
    "gpu-layers-draft",
    "n-gpu-layers-draft",
    "spec-draft-n-ubatch",
    "spec-draft-device",
    "devd",
    "device-draft",
    "spec-draft-type-k",
    "ctkd",
    "cache-type-k-draft",
    "spec-draft-type-v",
    "ctvd",
    "cache-type-v-draft",
    "spec-draft-override-tensor",
    "otd",
    "override-tensor-draft",
    "spec-draft-cpu-moe",
    "cpu-moe-draft",
    "cmoed",
    "spec-draft-n-cpu-moe",
    "spec-draft-ncmoe",
    "n-cpu-moe-draft",
    "ncmoed",
];

/// Hash of everything that affects a model's VRAM use.
pub fn memory_args_hash(model: &ModelConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(model.files.model.as_bytes());
    hasher.update([0]);
    hasher.update(model.files.mmproj.as_deref().unwrap_or("").as_bytes());
    // Only a model that has a draft file hashes it, so the hash of every other
    // model is what it was before speculative decoding existed and a learned
    // footprint survives an upgrade.
    if let Some(draft) = model.files.draft.as_deref().filter(|d| !d.trim().is_empty()) {
        hasher.update([0]);
        hasher.update(draft.as_bytes());
    }
    for &key in MEMORY_KEYS {
        if let Some(value) = model.llama_args.get(key) {
            hasher.update([0]);
            hasher.update(key.as_bytes());
            hasher.update(value.to_string().as_bytes());
        }
    }
    hex::encode(hasher.finalize())
}

/// Estimated footprint in MiB from the model file size plus the draft model
/// when one is used (spec §9.5, §10.6), or `None` if the model file cannot be
/// inspected.
pub fn estimate_for(model: &ModelConfig) -> Option<u64> {
    let bytes = std::fs::metadata(&model.files.model).ok()?.len();
    Some(estimate_footprint_mib(bytes) + draft_extra_mib(model))
}

/// The context size the estimate assumes: `ctx-size` when set and non-zero,
/// else the model's native context, else [`FALLBACK_CTX`].
fn assumed_ctx(model: &ModelConfig, native: Option<u32>) -> u32 {
    let set = model.llama_args.get("ctx-size").and_then(|v| match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    });
    set.filter(|c| *c > 0).and_then(|c| u32::try_from(c).ok()).or(native).unwrap_or(FALLBACK_CTX)
}

/// Extra MiB a speculative setup needs. One draft context serves every slot,
/// so it is counted once. A draft file counts its weights and its KV cache at
/// the assumed context; MTP without a file counts only the KV cache of the
/// model's prediction layers. A file that cannot be read adds nothing.
pub fn draft_extra_mib(model: &ModelConfig) -> u64 {
    let passed = draft_is_passed(model);
    let mtp = selected_types(&model.llama_args).contains(&SpecType::DraftMtp);
    if !passed && !mtp {
        // The usual case: no GGUF header is read for a model that does not use a draft.
        return 0;
    }
    let main = read_meta_cached(std::path::Path::new(&model.files.model)).ok();
    let ctx = assumed_ctx(model, main.as_ref().and_then(|m| m.context_length));
    let bytes = if passed {
        let path = model.files.draft.as_deref().unwrap_or_default();
        let weights = std::fs::metadata(path).map_or(0, |m| m.len());
        let cache =
            read_meta_cached(std::path::Path::new(path)).ok().and_then(|m| kv_cache_bytes(&m, ctx)).unwrap_or(0);
        weights + cache
    } else {
        main.and_then(|m| {
            let mut layers = (*m).clone();
            layers.block_count = m.nextn_predict_layers;
            kv_cache_bytes(&layers, ctx)
        })
        .unwrap_or(0)
    };
    bytes.div_ceil(BYTES_PER_MIB)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_changes_only_with_memory_relevant_settings() {
        let mut a = ModelConfig::default();
        a.files.model = "m.gguf".into();
        let base = memory_args_hash(&a);

        let mut sampling = a.clone();
        sampling.llama_args.insert("temp".into(), 0.7.into());
        assert_eq!(memory_args_hash(&sampling), base, "temperature does not change memory");

        let mut ctx = a.clone();
        ctx.llama_args.insert("ctx-size".into(), 32768.into());
        assert_ne!(memory_args_hash(&ctx), base);
    }

    /// A GGUF file with `layers` layers whose KV cache at 1,024 tokens is
    /// exactly `layers / 4` MiB (64-wide embedding, 4 heads, 4 KV heads).
    fn write_gguf(dir: &std::path::Path, name: &str, layers: u32, mtp_layers: Option<u32>) -> String {
        use crate::gguf::header::tests::{extend_gguf, sample_gguf};
        let uints: Vec<(String, u32)> =
            mtp_layers.map(|n| ("llama.nextn_predict_layers".to_owned(), n)).into_iter().collect();
        let uints: Vec<(&str, u32)> = uints.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        let path = dir.join(name);
        std::fs::write(&path, extend_gguf(sample_gguf("llama", layers, 1000, 64, 4, 4, 1), &uints, &[], &[]))
            .expect("write");
        path.to_string_lossy().into_owned()
    }

    fn with_ctx(mut m: ModelConfig, ctx: u32) -> ModelConfig {
        m.llama_args.insert("ctx-size".into(), ctx.into());
        m
    }

    /// The hash exactly as it was computed before speculative decoding existed.
    fn hash_before_speculative_decoding(model: &ModelConfig) -> String {
        const OLD_KEYS: [&str; 16] = [
            "ctx-size",
            "n-gpu-layers",
            "gpu-layers",
            "ngl",
            "cache-type-k",
            "cache-type-v",
            "batch-size",
            "ubatch-size",
            "parallel",
            "flash-attn",
            "kv-offload",
            "mmproj",
            "no-kv-offload",
            "tensor-split",
            "split-mode",
            "override-tensor",
        ];
        let mut hasher = Sha256::new();
        hasher.update(model.files.model.as_bytes());
        hasher.update([0]);
        hasher.update(model.files.mmproj.as_deref().unwrap_or("").as_bytes());
        for key in OLD_KEYS {
            if let Some(value) = model.llama_args.get(key) {
                hasher.update([0]);
                hasher.update(key.as_bytes());
                hasher.update(value.to_string().as_bytes());
            }
        }
        hex::encode(hasher.finalize())
    }

    #[test]
    fn hash_is_unchanged_for_models_without_a_draft() {
        // A learned footprint is stored under this hash; changing it for existing
        // models would discard every measurement on upgrade.
        let mut m = ModelConfig::default();
        m.files.model = "C:/models/m.gguf".into();
        assert_eq!(memory_args_hash(&m), hash_before_speculative_decoding(&m));
        m.files.mmproj = Some("C:/models/mmproj.gguf".into());
        m.llama_args.insert("ctx-size".into(), 8192.into());
        m.llama_args.insert("n-gpu-layers".into(), "auto".into());
        m.llama_args.insert("temp".into(), 0.7.into());
        assert_eq!(memory_args_hash(&m), hash_before_speculative_decoding(&m));
        m.files.draft = Some("  ".into());
        assert_eq!(memory_args_hash(&m), hash_before_speculative_decoding(&m), "a blank draft path counts as none");
    }

    #[test]
    fn no_work_and_no_extra_memory_without_speculative_decoding() {
        let mut m = ModelConfig::default();
        m.files.model = "not-here.gguf".into();
        assert_eq!(draft_extra_mib(&m), 0);
        m.llama_args.insert("spec-type".into(), "ngram-mod".into());
        assert_eq!(draft_extra_mib(&m), 0, "n-gram types need no draft memory");
    }

    #[test]
    fn draft_changes_the_hash() {
        let mut a = ModelConfig::default();
        a.files.model = "m.gguf".into();
        let base = memory_args_hash(&a);
        a.files.draft = Some("d.gguf".into());
        assert_ne!(memory_args_hash(&a), base);
    }

    #[test]
    fn speculative_tuning_that_changes_memory_changes_the_hash_but_draft_n_max_does_not() {
        let mut a = ModelConfig::default();
        a.files.model = "m.gguf".into();
        let base = memory_args_hash(&a);
        for (key, value) in [("spec-type", "draft-simple"), ("spec-draft-ngl", "10"), ("ngld", "all"), ("ctkd", "q8_0")]
        {
            let mut m = a.clone();
            m.llama_args.insert(key.into(), value.into());
            assert_ne!(memory_args_hash(&m), base, "{key}");
        }
        for key in ["spec-draft-n-max", "spec-draft-n-min", "spec-draft-p-min"] {
            let mut m = a.clone();
            m.llama_args.insert(key.into(), 4.into());
            assert_eq!(memory_args_hash(&m), base, "{key} does not change memory");
        }
    }

    #[test]
    fn estimate_adds_the_draft_once() {
        let dir = tempfile::tempdir().expect("dir");
        let mut m = with_ctx(ModelConfig::default(), 1024);
        m.files.model = write_gguf(dir.path(), "main.gguf", 4, None);
        let plain = estimate_for(&m).expect("estimate");
        m.files.draft = Some(write_gguf(dir.path(), "draft.gguf", 4, None));
        m.llama_args.insert("spec-type".into(), "draft-simple".into());
        let draft_bytes = std::fs::metadata(m.files.draft.as_deref().unwrap_or_default()).expect("meta").len();
        // 4 layers at 1,024 tokens is 1 MiB of cache, plus the (tiny) weights.
        assert_eq!(estimate_for(&m).expect("estimate"), plain + (draft_bytes + BYTES_PER_MIB).div_ceil(BYTES_PER_MIB));
        // Two draft types still need one draft context.
        m.llama_args.insert("spec-type".into(), "draft-simple,draft-mtp".into());
        assert_eq!(estimate_for(&m).expect("estimate"), plain + (draft_bytes + BYTES_PER_MIB).div_ceil(BYTES_PER_MIB));
    }

    #[test]
    fn estimate_ignores_an_unused_draft() {
        let dir = tempfile::tempdir().expect("dir");
        let mut m = with_ctx(ModelConfig::default(), 1024);
        m.files.model = write_gguf(dir.path(), "main.gguf", 4, None);
        let plain = estimate_for(&m).expect("estimate");
        m.files.draft = Some(write_gguf(dir.path(), "draft.gguf", 4, None));
        assert_eq!(estimate_for(&m), Some(plain), "Off: the file is not passed, so it needs no memory");
        m.llama_args.insert("spec-type".into(), "ngram-mod".into());
        assert_eq!(estimate_for(&m), Some(plain));
    }

    #[test]
    fn mtp_without_file_adds_only_prediction_layer_cache() {
        let dir = tempfile::tempdir().expect("dir");
        let mut m = with_ctx(ModelConfig::default(), 1024);
        m.files.model = write_gguf(dir.path(), "main.gguf", 8, Some(4));
        let plain = estimate_for(&m).expect("estimate");
        m.llama_args.insert("spec-type".into(), "draft-mtp".into());
        // 4 prediction layers at 1,024 tokens: exactly 1 MiB.
        assert_eq!(estimate_for(&m), Some(plain + 1));
        m.files.model = write_gguf(dir.path(), "plain.gguf", 8, None);
        let with_spec = estimate_for(&m);
        m.llama_args.remove("spec-type");
        assert_eq!(with_spec, estimate_for(&m), "no prediction layers, nothing to add");
    }

    #[test]
    fn estimate_is_none_for_missing_file() {
        let mut m = ModelConfig::default();
        m.files.model = "definitely-not-here.gguf".into();
        assert_eq!(estimate_for(&m), None);
    }
}
