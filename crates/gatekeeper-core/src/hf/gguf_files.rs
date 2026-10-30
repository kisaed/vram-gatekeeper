//! Grouping a repository's files into things a person can choose (spec §15.2).
//!
//! Responsibility: turn a flat file list into quantisation variants (one file
//! or a complete split set), separate vision projectors, and judge whether a
//! variant will fit the GPU.
//! Not responsible for: talking to Hugging Face (`api`) or downloading.
//!
//! Implements FR-MOD-4.

use serde::Serialize;

use super::api::RepoFile;
use crate::gate::vram_rules::ESTIMATE_COMPUTE_MARGIN_MIB;
use crate::gpu::BYTES_PER_MIB;

/// What a variant consists of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VariantKind {
    /// One `.gguf` file.
    Single,
    /// Several `-0000N-of-0000M.gguf` shards that must all be downloaded.
    Split,
}

/// One downloadable choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Variant {
    /// Stable id: the file (or shard prefix) path without extension.
    pub id: String,
    /// Quantisation label such as `Q4_K_M`, or `unknown`.
    pub quant: String,
    /// Single file or split set.
    pub kind: VariantKind,
    /// The files to download, first shard first.
    pub files: Vec<RepoFile>,
    /// Total size in bytes.
    pub total_bytes: u64,
    /// False if some shards of a split set are missing from the repository.
    pub complete: bool,
}

/// A repository's files, grouped.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Grouped {
    /// Model variants, smallest first.
    pub variants: Vec<Variant>,
    /// Vision projector files (`mmproj*.gguf`).
    pub mmproj: Vec<RepoFile>,
    /// Draft sidecar files (`mtp-*`, `dflash-*`, `dspark-*`), offered with a
    /// variant like a projector (spec §10.6).
    pub sidecars: Vec<RepoFile>,
}

/// Known quantisation labels, longest first so `q4_k_m` wins over `q4_k`.
const QUANTS: [&str; 43] = [
    "iq2_xxs", "iq3_xxs", "iq1_s", "iq1_m", "iq2_xs", "iq2_s", "iq2_m", "iq3_xs", "iq3_s", "iq3_m", "iq4_xs", "iq4_nl",
    "q2_k_s", "q3_k_s", "q3_k_m", "q3_k_l", "q4_k_s", "q4_k_m", "q5_k_s", "q5_k_m", "mxfp4", "tq1_0", "tq2_0", "q2_k",
    "q3_k", "q4_k", "q5_k", "q6_k", "q4_0", "q4_1", "q5_0", "q5_1", "q8_0", "q8_1", "bf16", "fp16", "fp32", "f16",
    "f32", "q4_k_xl", "q5_k_xl", "q6_k_xl", "q8_k_xl",
];

/// Extracts the quantisation label from a file name (case-insensitive).
pub fn quant_of(file_name: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    let mut best: Option<&str> = None;
    for q in QUANTS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(q) {
            let start = from + pos;
            let end = start + q.len();
            let before_ok = start == 0 || !lower.as_bytes()[start - 1].is_ascii_alphanumeric();
            let after_ok =
                end >= lower.len() || !lower.as_bytes()[end].is_ascii_alphanumeric() && lower.as_bytes()[end] != b'_';
            if before_ok && after_ok && best.is_none_or(|b| q.len() > b.len()) {
                best = Some(q);
            }
            from = start + 1;
        }
    }
    best.map_or_else(|| "unknown".to_owned(), str::to_ascii_uppercase)
}

/// Parses `<prefix>-00001-of-00003.gguf` into `(prefix, index, total)`.
pub(crate) fn parse_shard(path: &str) -> Option<(String, u32, u32)> {
    let stem = path.strip_suffix(".gguf").or_else(|| path.strip_suffix(".GGUF"))?;
    let (rest, total) = stem.rsplit_once("-of-")?;
    let (prefix, index) = rest.rsplit_once('-')?;
    if index.len() != 5 || total.len() != 5 {
        return None;
    }
    Some((prefix.to_owned(), index.parse().ok()?, total.parse().ok()?))
}

/// True for a multimodal projector file (`mmproj-*.gguf`).
pub(crate) fn is_projector_name(name: &str) -> bool {
    name.to_ascii_lowercase().contains("mmproj")
}

/// True for a draft sidecar: the prefixes the engine's own downloader looks
/// for (`mtp-`, `dflash-`, `dspark-`).
pub(crate) fn is_sidecar_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    ["mtp-", "dflash-", "dspark-"].iter().any(|p| lower.starts_with(p))
}

/// File name without folders.
pub(crate) fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Shards collected for one `<prefix>-of-<total>` set: `(prefix, total, [(index, file)])`.
type ShardSet = (String, u32, Vec<(u32, RepoFile)>);

/// Groups `.gguf` files into variants and projectors.
pub fn group_files(files: &[RepoFile]) -> Grouped {
    let mut grouped = Grouped::default();
    let mut shard_sets: Vec<ShardSet> = Vec::new();

    for f in files.iter().filter(|f| f.path.to_ascii_lowercase().ends_with(".gguf")) {
        if is_projector_name(base_name(&f.path)) {
            grouped.mmproj.push(f.clone());
        } else if is_sidecar_name(base_name(&f.path)) {
            grouped.sidecars.push(f.clone());
        } else if let Some((prefix, index, total)) = parse_shard(&f.path) {
            match shard_sets.iter_mut().find(|(p, t, _)| *p == prefix && *t == total) {
                Some((_, _, list)) => list.push((index, f.clone())),
                None => shard_sets.push((prefix, total, vec![(index, f.clone())])),
            }
        } else {
            grouped.variants.push(Variant {
                id: f.path.trim_end_matches(".gguf").to_owned(),
                quant: quant_of(base_name(&f.path)),
                kind: VariantKind::Single,
                files: vec![f.clone()],
                total_bytes: f.size,
                complete: true,
            });
        }
    }

    for (prefix, total, mut list) in shard_sets {
        list.sort_by_key(|(i, _)| *i);
        let complete = list.len() as u32 == total && list.iter().enumerate().all(|(n, (i, _))| *i == n as u32 + 1);
        let files: Vec<RepoFile> = list.into_iter().map(|(_, f)| f).collect();
        grouped.variants.push(Variant {
            quant: quant_of(base_name(&prefix)),
            id: prefix,
            kind: VariantKind::Split,
            total_bytes: files.iter().map(|f| f.size).sum(),
            files,
            complete,
        });
    }

    grouped.variants.sort_by_key(|v| v.total_bytes);
    grouped.mmproj.sort_by_key(|f| f.size);
    grouped.sidecars.sort_by_key(|f| f.size);
    grouped
}

/// How well a variant is expected to fit the monitored GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Expected to sit entirely in VRAM with a modest context.
    FitsFully,
    /// Larger than VRAM; llama.cpp would keep some layers on the CPU.
    PartialCpuOffload,
    /// So large that even with offload it is impractical.
    TooBig,
}

/// Fraction of the weights' size added for the KV cache and compute buffers of
/// a modest (8k) context when real metadata is not available yet.
const OVERHEAD_PERCENT: u64 = 20;

/// Largest multiple of VRAM a model may be before it is called too big (the
/// remainder would have to live in system RAM, where it runs very slowly).
const MAX_OFFLOAD_MULTIPLE: u64 = 3;

/// Judges fit from the file size and the GPU's total VRAM.
pub fn fit_for(file_bytes: u64, total_vram_mib: u64) -> Fit {
    if total_vram_mib == 0 {
        return Fit::PartialCpuOffload;
    }
    let file_mib = file_bytes / BYTES_PER_MIB;
    let need = file_mib + file_mib * OVERHEAD_PERCENT / 100 + ESTIMATE_COMPUTE_MARGIN_MIB;
    if need <= total_vram_mib * 95 / 100 {
        Fit::FitsFully
    } else if file_mib <= total_vram_mib * MAX_OFFLOAD_MULTIPLE {
        Fit::PartialCpuOffload
    } else {
        Fit::TooBig
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile { path: path.into(), size, sha256: Some(format!("sha-{path}")) }
    }

    #[test]
    fn quant_labels_prefer_the_longest_match() {
        assert_eq!(quant_of("Qwen3-8B-Q4_K_M.gguf"), "Q4_K_M");
        assert_eq!(quant_of("model-IQ4_XS.gguf"), "IQ4_XS");
        assert_eq!(quant_of("llama-3.2-1b-instruct-Q8_0.gguf"), "Q8_0");
        assert_eq!(quant_of("thing.BF16.gguf"), "BF16");
        assert_eq!(quant_of("model-f16.gguf"), "F16");
        assert_eq!(quant_of("qwen2.5-0.5b-instruct-fp16.gguf"), "FP16");
        assert_eq!(quant_of("something-else.gguf"), "unknown");
        // `q4_0` inside a longer token must not match.
        assert_eq!(quant_of("xq4_0y.gguf"), "unknown");
    }

    #[test]
    fn singles_are_sorted_smallest_first() {
        let g = group_files(&[file("m-Q8_0.gguf", 800), file("m-Q4_K_M.gguf", 400), file("README.md", 1)]);
        assert_eq!(g.variants.iter().map(|v| v.quant.as_str()).collect::<Vec<_>>(), ["Q4_K_M", "Q8_0"]);
        assert!(g.variants.iter().all(|v| v.kind == VariantKind::Single && v.complete));
    }

    #[test]
    fn split_models_are_one_variant_with_the_total_size() {
        let g = group_files(&[
            file("Q4_K_M/big-Q4_K_M-00002-of-00003.gguf", 100),
            file("Q4_K_M/big-Q4_K_M-00001-of-00003.gguf", 100),
            file("Q4_K_M/big-Q4_K_M-00003-of-00003.gguf", 50),
            file("small-Q2_K.gguf", 10),
        ]);
        assert_eq!(g.variants.len(), 2);
        let split = g.variants.iter().find(|v| v.kind == VariantKind::Split).expect("split variant");
        assert_eq!((split.total_bytes, split.files.len(), split.complete), (250, 3, true));
        assert!(split.files[0].path.contains("00001"), "first shard first: llama.cpp loads that one");
        assert_eq!(split.quant, "Q4_K_M");
    }

    #[test]
    fn incomplete_split_sets_are_flagged() {
        let g = group_files(&[file("x-00001-of-00003.gguf", 1), file("x-00003-of-00003.gguf", 1)]);
        assert!(!g.variants[0].complete);
    }

    #[test]
    fn projectors_are_listed_separately() {
        let g = group_files(&[
            file("model-Q4_K_M.gguf", 10),
            file("mmproj-model-f16.gguf", 2),
            file("sub/mmproj-F32.gguf", 4),
        ]);
        assert_eq!(g.variants.len(), 1);
        assert_eq!(g.mmproj.iter().map(|f| f.size).collect::<Vec<_>>(), [2, 4]);
    }

    #[test]
    fn sidecars_are_separated_from_variants() {
        let g = group_files(&[
            file("Qwen3-8B-Q4_K_M.gguf", 8),
            file("MTP-Qwen3-8B-Q4_K_M.gguf", 1),
            file("sub/dflash-qwen3.gguf", 3),
            file("dspark-qwen3.gguf", 2),
            file("eagle3-draft.gguf", 5),
        ]);
        assert_eq!(g.sidecars.iter().map(|f| f.size).collect::<Vec<_>>(), [1, 2, 3]);
        let ids: Vec<&str> = g.variants.iter().map(|v| v.id.as_str()).collect();
        assert_eq!(ids, ["eagle3-draft", "Qwen3-8B-Q4_K_M"], "EAGLE-3 drafts have no prefix and stay ordinary files");
    }

    #[test]
    fn fit_follows_size_against_vram() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(fit_for(4 * gib, 32 * 1024), Fit::FitsFully);
        assert_eq!(fit_for(30 * gib, 32 * 1024), Fit::PartialCpuOffload, "weights plus cache exceed VRAM");
        assert_eq!(fit_for(60 * gib, 32 * 1024), Fit::PartialCpuOffload);
        assert_eq!(fit_for(200 * gib, 32 * 1024), Fit::TooBig);
        assert_eq!(fit_for(gib, 0), Fit::PartialCpuOffload, "unknown VRAM is not judged");
    }
}
