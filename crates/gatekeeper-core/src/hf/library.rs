//! The model library: files on disk, which models use them, and turning a
//! finished download into a model entry (spec §15.4, §15.5).
//!
//! Responsibility: scan the models folder, flag unused ("orphan") files, delete
//! files safely, and build a sensible [`ModelConfig`] for a new download using
//! the GGUF metadata.
//! Not responsible for downloading (`queue`) or for saving the configuration.
//!
//! Implements FR-MOD-4.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::queue::DownloadSpec;
use crate::config::schema::{Config, ModelConfig, ModelFiles, ModelSource};
use crate::gguf::header::read_meta;
use crate::hf::gguf_files::{base_name, is_projector_name, is_sidecar_name, parse_shard, quant_of};
use crate::speculative::{kind_from_file_name, with_type_added};

/// Default context size for new models: keeps VRAM use predictable instead of
/// using the model's full native context (spec §15.4).
pub const DEFAULT_CTX: u32 = 8192;

/// One GGUF file in the library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LibraryFile {
    /// Full path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Quantisation guessed from the file name.
    pub quant: String,
    /// Ids of models that use this file.
    pub used_by: Vec<String>,
    /// True for vision projector files.
    pub is_projector: bool,
}

/// Normalises a path for comparison (Windows paths are case-insensitive and
/// accept both separators).
fn norm(p: &str) -> String {
    p.replace('\\', "/").to_ascii_lowercase()
}

/// Every `.gguf` under `dir`, recursively, skipping `.part` files.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("gguf")) {
            out.push(p);
        }
    }
}

/// Shard prefix of `<prefix>-00001-of-00003.gguf`, if the name has that shape.
fn shard_prefix(name: &str) -> Option<String> {
    parse_shard(name).map(|(prefix, _, _)| prefix)
}

/// A file's folder (normalised) and lower-cased shard prefix, if it is a shard.
type ShardKey = (Option<String>, String);

/// True if the configured model file `used` is the file at `here` or, for a
/// split model that lists only its first shard, one of its siblings.
fn is_used_file(used: &str, here: &str, shard_here: Option<&ShardKey>) -> bool {
    if norm(used) == here {
        return true;
    }
    let used_path = Path::new(used);
    let used_name = file_name_of(used_path);
    match (shard_prefix(&used_name), shard_here) {
        (Some(up), Some((dir, hp))) => {
            up.to_ascii_lowercase() == *hp && used_path.parent().map(|d| norm(&d.to_string_lossy())) == *dir
        }
        _ => false,
    }
}

/// The last component of `path` as text.
fn file_name_of(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// Lists the library with usage information.
pub fn scan(models_dir: &Path, cfg: &Config) -> Vec<LibraryFile> {
    let mut paths = Vec::new();
    walk(models_dir, &mut paths);
    paths.sort();

    let mut files = Vec::new();
    for p in paths {
        let path_str = p.to_string_lossy().into_owned();
        let name = file_name_of(&p);
        let here = norm(&path_str);
        let shard_here: Option<ShardKey> =
            shard_prefix(&name).map(|s| (p.parent().map(|d| norm(&d.to_string_lossy())), s.to_ascii_lowercase()));

        let used_by = cfg
            .models
            .iter()
            .filter(|m| {
                [Some(&m.files.model), m.files.mmproj.as_ref(), m.files.draft.as_ref()]
                    .into_iter()
                    .flatten()
                    .any(|used| is_used_file(used, &here, shard_here.as_ref()))
            })
            .map(|m| m.id.clone())
            .collect();

        files.push(LibraryFile {
            size: std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0),
            quant: quant_of(&name),
            path: path_str,
            used_by,
            is_projector: is_projector_name(&name),
        });
    }
    files
}

/// Deletes a library file (and, for a split model, all of its shards).
///
/// # Errors
/// Refuses paths outside `models_dir` and files in use by a model; otherwise
/// returns the I/O error.
pub fn delete(models_dir: &Path, cfg: &Config, path: &Path) -> Result<usize, String> {
    let root = std::fs::canonicalize(models_dir).map_err(|e| format!("cannot open the models folder: {e}"))?;
    let target = std::fs::canonicalize(path).map_err(|e| format!("cannot find the file: {e}"))?;
    if !target.starts_with(&root) {
        return Err("That file is not inside the models folder.".to_owned());
    }
    let library = scan(models_dir, cfg);
    let target_str = target.to_string_lossy().replace("\\\\?\\", "");
    let entry = library.iter().find(|f| norm(&f.path) == norm(&target_str));
    if let Some(e) = entry
        && !e.used_by.is_empty()
    {
        return Err(format!("It is used by {}. Remove or change that model first.", e.used_by.join(", ")));
    }
    // Delete siblings of a split set together.
    let name = file_name_of(&target);
    let mut doomed = vec![target.clone()];
    if let (Some(prefix), Some(dir)) = (shard_prefix(&name), target.parent())
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        doomed = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).and_then(shard_prefix).as_ref() == Some(&prefix))
            .collect();
    }
    let mut removed = 0;
    for p in doomed {
        std::fs::remove_file(&p).map_err(|e| format!("could not delete {}: {e}", p.display()))?;
        removed += 1;
    }
    Ok(removed)
}

/// Turns `text` into a valid, unique model id.
fn unique_id(text: &str, taken: &[String]) -> String {
    let base: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-') { c } else { '-' })
        .collect();
    let base = base.trim_matches('-');
    let base: String = base.chars().take(60).collect();
    let base = if base.is_empty() { "model".to_owned() } else { base };
    let lower: Vec<String> = taken.iter().map(|t| t.to_ascii_lowercase()).collect();
    if !lower.contains(&base.to_ascii_lowercase()) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|c| !lower.contains(&c.to_ascii_lowercase())).unwrap_or(base)
}

/// Builds the model entry for a finished download (spec §15.4).
pub fn model_from_download(spec: &DownloadSpec, paths: &[PathBuf], existing_ids: &[String]) -> ModelConfig {
    let (projectors, rest): (Vec<&PathBuf>, Vec<&PathBuf>) =
        paths.iter().partition(|p| is_projector_name(&file_name_of(p)));
    let (sidecars, models): (Vec<&PathBuf>, Vec<&PathBuf>) =
        rest.into_iter().partition(|p| is_sidecar_name(&file_name_of(p)));
    let main = models.first().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let quant = models.first().and_then(|p| p.file_name()).map(|n| quant_of(&n.to_string_lossy())).unwrap_or_default();

    let repo_name = base_name(&spec.repo);
    let id_text =
        if quant == "unknown" || quant.is_empty() { repo_name.to_owned() } else { format!("{repo_name}-{quant}") };

    // Cap the context so VRAM use stays predictable, but never above what the model supports.
    let native = models.first().and_then(|p| read_meta(p).ok()).and_then(|m| m.context_length);
    let ctx = native.map_or(DEFAULT_CTX, |n| n.min(DEFAULT_CTX));

    let mut model = ModelConfig {
        id: unique_id(&id_text, existing_ids),
        display_name: spec.label.clone(),
        files: ModelFiles {
            model: main,
            mmproj: projectors.first().map(|p| p.to_string_lossy().into_owned()),
            draft: sidecars.first().map(|p| p.to_string_lossy().into_owned()),
        },
        source: Some(ModelSource::Huggingface {
            repo: spec.repo.clone(),
            revision: spec.revision.clone(),
            commit_sha: spec.commit_sha.clone(),
            files: spec.files.iter().map(|f| f.path.clone()).collect(),
        }),
        ..ModelConfig::default()
    };
    model.llama_args.insert("ctx-size".to_owned(), ctx.into());
    // A sidecar downloaded with the model is its draft, with the matching type (spec §10.6).
    if let Some(ty) = sidecars.first().and_then(|p| kind_from_file_name(&file_name_of(p))) {
        with_type_added(&mut model.llama_args, ty);
    }
    model
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hf::api::RepoFile;

    fn spec(files: &[&str]) -> DownloadSpec {
        DownloadSpec {
            repo: "Qwen/Qwen3-8B-GGUF".into(),
            revision: "main".into(),
            commit_sha: Some("abc".into()),
            files: files.iter().map(|f| RepoFile { path: (*f).into(), size: 1, sha256: None }).collect(),
            label: "Qwen3 8B (Q4_K_M)".into(),
        }
    }

    #[test]
    fn new_model_gets_id_source_and_a_capped_context() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("Qwen3-8B-Q4_K_M.gguf");
        std::fs::write(&file, crate::gguf::header::tests::sample_gguf("qwen3", 36, 40960, 4096, 32, 8, 15))
            .expect("write");
        let m = model_from_download(&spec(&["Qwen3-8B-Q4_K_M.gguf"]), std::slice::from_ref(&file), &[]);
        assert_eq!(m.id, "Qwen3-8B-GGUF-Q4_K_M");
        assert_eq!(m.display_name, "Qwen3 8B (Q4_K_M)");
        assert_eq!(m.files.model, file.to_string_lossy());
        assert_eq!(m.llama_args["ctx-size"], DEFAULT_CTX, "native 40960 is capped to 8192");
        assert!(
            matches!(m.source, Some(ModelSource::Huggingface { ref commit_sha, .. }) if commit_sha.as_deref() == Some("abc"))
        );
    }

    #[test]
    fn small_native_context_is_respected_and_ids_stay_unique() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("tiny-Q8_0.gguf");
        std::fs::write(&file, crate::gguf::header::tests::sample_gguf("llama", 2, 2048, 64, 4, 4, 7)).expect("write");
        let taken = vec!["Qwen3-8B-GGUF-Q8_0".to_owned()];
        let m = model_from_download(&spec(&["tiny-Q8_0.gguf"]), &[file], &taken);
        assert_eq!(m.id, "Qwen3-8B-GGUF-Q8_0-2");
        assert_eq!(m.llama_args["ctx-size"], 2048);
    }

    #[test]
    fn unreadable_metadata_falls_back_to_the_default_context() {
        let dir = tempfile::tempdir().expect("dir");
        let file = dir.path().join("x-Q4_K_M.gguf");
        std::fs::write(&file, b"not a gguf").expect("write");
        assert_eq!(model_from_download(&spec(&["x-Q4_K_M.gguf"]), &[file], &[]).llama_args["ctx-size"], DEFAULT_CTX);
    }

    #[test]
    fn projector_files_become_the_mmproj() {
        let a = PathBuf::from(r"C:\M\m-Q4_K_M.gguf");
        let b = PathBuf::from(r"C:\M\mmproj-F16.gguf");
        let m = model_from_download(&spec(&["m-Q4_K_M.gguf", "mmproj-F16.gguf"]), &[a, b.clone()], &[]);
        assert_eq!(m.files.mmproj.as_deref(), Some(b.to_string_lossy().as_ref()));
    }

    #[test]
    fn scan_marks_used_and_orphan_files_and_split_siblings() {
        let dir = tempfile::tempdir().expect("dir");
        let d = dir.path().join("a").join("r");
        std::fs::create_dir_all(&d).expect("mkdir");
        for n in [
            "used-Q4_K_M.gguf",
            "orphan-Q8_0.gguf",
            "big-00001-of-00002.gguf",
            "big-00002-of-00002.gguf",
            "stray.gguf.part",
        ] {
            std::fs::write(d.join(n), b"x").expect("write");
        }
        let mut cfg = Config::default();
        cfg.models.push(ModelConfig {
            id: "one".into(),
            files: ModelFiles { model: d.join("used-Q4_K_M.gguf").to_string_lossy().into(), mmproj: None, draft: None },
            ..ModelConfig::default()
        });
        cfg.models.push(ModelConfig {
            id: "two".into(),
            files: ModelFiles {
                model: d.join("big-00001-of-00002.gguf").to_string_lossy().into(),
                mmproj: None,
                draft: None,
            },
            ..ModelConfig::default()
        });
        let lib = scan(dir.path(), &cfg);
        assert_eq!(lib.len(), 4, "the .part file is not listed");
        let used = |n: &str| lib.iter().find(|f| f.path.ends_with(n)).expect("file").used_by.clone();
        assert_eq!(used("used-Q4_K_M.gguf"), ["one"]);
        assert!(used("orphan-Q8_0.gguf").is_empty());
        assert_eq!(
            used("big-00002-of-00002.gguf"),
            ["two"],
            "the second shard counts as used by the model that lists the first"
        );
    }

    #[test]
    fn sidecar_download_becomes_the_draft_with_its_type() {
        let a = PathBuf::from(r"C:\M\Qwen3-8B-Q4_K_M.gguf");
        let b = PathBuf::from(r"C:\M\mtp-Qwen3-8B-Q4_K_M.gguf");
        let m = model_from_download(
            &spec(&["Qwen3-8B-Q4_K_M.gguf", "mtp-Qwen3-8B-Q4_K_M.gguf"]),
            &[a.clone(), b.clone()],
            &[],
        );
        assert_eq!(m.files.model, a.to_string_lossy());
        assert_eq!(m.files.draft.as_deref(), Some(b.to_string_lossy().as_ref()));
        assert_eq!(m.llama_args["spec-type"], "draft-mtp");
    }

    #[test]
    fn draft_file_counts_as_a_use() {
        let dir = tempfile::tempdir().expect("dir");
        for n in ["main.gguf", "draft.gguf"] {
            std::fs::write(dir.path().join(n), b"x").expect("write");
        }
        let mut cfg = Config::default();
        cfg.models.push(ModelConfig {
            id: "m".into(),
            files: ModelFiles {
                model: dir.path().join("main.gguf").to_string_lossy().into(),
                mmproj: None,
                draft: Some(dir.path().join("draft.gguf").to_string_lossy().into()),
            },
            ..ModelConfig::default()
        });
        let lib = scan(dir.path(), &cfg);
        let draft = lib.iter().find(|f| f.path.ends_with("draft.gguf")).expect("draft listed");
        assert_eq!(draft.used_by, ["m"]);
    }

    #[test]
    fn delete_refuses_a_file_used_as_a_draft() {
        let dir = tempfile::tempdir().expect("dir");
        let draft = dir.path().join("draft.gguf");
        std::fs::write(&draft, b"x").expect("write");
        let mut cfg = Config::default();
        cfg.models.push(ModelConfig {
            id: "m".into(),
            files: ModelFiles { model: "main.gguf".into(), mmproj: None, draft: Some(draft.to_string_lossy().into()) },
            ..ModelConfig::default()
        });
        assert!(delete(dir.path(), &cfg, &draft).expect_err("in use").contains("used by m"));
        assert!(draft.exists());
    }

    #[test]
    fn delete_refuses_used_files_and_outside_paths_and_removes_all_shards() {
        let dir = tempfile::tempdir().expect("dir");
        let outside = tempfile::tempdir().expect("outside");
        let used = dir.path().join("used.gguf");
        let s1 = dir.path().join("big-00001-of-00002.gguf");
        let s2 = dir.path().join("big-00002-of-00002.gguf");
        for p in [&used, &s1, &s2] {
            std::fs::write(p, b"x").expect("write");
        }
        let stray = outside.path().join("other.gguf");
        std::fs::write(&stray, b"x").expect("write");
        let mut cfg = Config::default();
        cfg.models.push(ModelConfig {
            id: "m".into(),
            files: ModelFiles { model: used.to_string_lossy().into(), mmproj: None, draft: None },
            ..ModelConfig::default()
        });

        assert!(delete(dir.path(), &cfg, &used).expect_err("in use").contains("used by m"));
        assert!(delete(dir.path(), &cfg, &stray).expect_err("outside").contains("not inside"));
        assert_eq!(delete(dir.path(), &cfg, &s1).expect("deleted"), 2, "both shards go together");
        assert!(!s1.exists() && !s2.exists() && used.exists());
    }
}
