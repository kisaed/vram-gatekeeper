//! Model update checks (spec §15.5, Phase 5).
//!
//! Responsibility: decide whether a downloaded model has a newer version on
//! Hugging Face by comparing the commit recorded at download time with the
//! repository's current commit, and, when they differ, whether the model's own
//! files changed (a repository moves on for README edits too).
//! Not responsible for: downloading the update. A loaded model holds its files
//! open, so replacing them is left to the user (spec: update checks are opt-in).
//!
//! Implements FR-MOD-4 (update checks).

use std::path::Path;

use serde::Serialize;

use super::api::RepoListing;
use super::gguf_files::base_name;

/// What the check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Same commit as the download.
    UpToDate,
    /// The repository changed, but this model's files look the same.
    RepoChanged,
    /// One or more of this model's files differ from the repository's.
    FilesChanged,
    /// Could not tell (no recorded commit, offline, gated, removed).
    Unknown,
}

/// The result for one model, as the UI shows it.
#[derive(Debug, Clone, Serialize)]
pub struct ModelUpdate {
    /// Model id.
    pub model_id: String,
    /// Hugging Face repository.
    pub repo: String,
    /// The verdict.
    pub verdict: Verdict,
    /// Current commit of the repository, if it was read.
    pub latest_sha: Option<String>,
    /// A sentence for the user.
    pub message: String,
}

/// Judges one model.
///
/// `source_files` are the repo-relative file names recorded at download time;
/// `local_size` returns the size of the local copy of one of them.
pub fn judge(
    stored_sha: Option<&str>,
    source_files: &[String],
    listing: &RepoListing,
    local_size: impl Fn(&str) -> Option<u64>,
) -> (Verdict, String) {
    let Some(stored) = stored_sha.filter(|s| !s.is_empty()) else {
        return (
            Verdict::Unknown,
            "No version was recorded when this model was added, so there is nothing to compare.".to_owned(),
        );
    };
    let Some(latest) = listing.sha.as_deref() else {
        return (Verdict::Unknown, "Hugging Face did not say which version is current.".to_owned());
    };
    if latest == stored {
        return (Verdict::UpToDate, "This is the latest version.".to_owned());
    }
    let changed: Vec<&String> = source_files
        .iter()
        .filter(|name| {
            let remote = listing.files.iter().find(|f| &f.path == *name).map(|f| f.size);
            // A file that vanished from the repository, or whose size differs
            // from the local copy, counts as changed.
            match (remote, local_size(name)) {
                (Some(r), Some(l)) => r != l,
                (None, _) => true,
                (Some(_), None) => false,
            }
        })
        .collect();
    if changed.is_empty() {
        (
            Verdict::RepoChanged,
            "The repository has new commits, but this model's files look the same. You probably do not need to do anything."
                .to_owned(),
        )
    } else {
        (
            Verdict::FilesChanged,
            format!(
                "A newer version of {} is available. Download it again from the Download page; your settings for this model stay.",
                changed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            ),
        )
    }
}

/// Size of the local copy of repo file `relative`, looked up next to the
/// model's own files (shards and projectors live beside each other).
pub fn local_size_near(model_files: &[&Path], relative: &str) -> Option<u64> {
    let name = base_name(relative);
    model_files
        .iter()
        .filter_map(|f| f.parent())
        .find_map(|dir| std::fs::metadata(dir.join(name)).ok().filter(|m| m.is_file()).map(|m| m.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hf::api::RepoFile;

    fn listing(sha: Option<&str>, files: &[(&str, u64)]) -> RepoListing {
        RepoListing {
            sha: sha.map(str::to_owned),
            files: files.iter().map(|(p, s)| RepoFile { path: (*p).into(), size: *s, sha256: None }).collect(),
        }
    }

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn same_commit_is_up_to_date() {
        let (v, _) = judge(Some("abc"), &files(&["m.gguf"]), &listing(Some("abc"), &[("m.gguf", 10)]), |_| Some(10));
        assert_eq!(v, Verdict::UpToDate);
    }

    #[test]
    fn a_new_commit_with_identical_files_is_only_a_repo_change() {
        let (v, msg) = judge(Some("abc"), &files(&["m.gguf"]), &listing(Some("def"), &[("m.gguf", 10)]), |_| Some(10));
        assert_eq!(v, Verdict::RepoChanged);
        assert!(msg.contains("do not need"), "{msg}");
    }

    #[test]
    fn a_different_size_or_a_removed_file_means_the_model_changed() {
        let (v, msg) = judge(Some("abc"), &files(&["m.gguf"]), &listing(Some("def"), &[("m.gguf", 11)]), |_| Some(10));
        assert_eq!(v, Verdict::FilesChanged);
        assert!(msg.contains("m.gguf"), "{msg}");
        let (gone, _) = judge(Some("abc"), &files(&["m.gguf"]), &listing(Some("def"), &[]), |_| Some(10));
        assert_eq!(gone, Verdict::FilesChanged);
    }

    #[test]
    fn a_missing_local_file_is_not_called_changed() {
        // Maybe the user moved it; the size comparison cannot be made.
        let (v, _) = judge(Some("abc"), &files(&["m.gguf"]), &listing(Some("def"), &[("m.gguf", 10)]), |_| None);
        assert_eq!(v, Verdict::RepoChanged);
    }

    #[test]
    fn missing_information_is_unknown_not_a_guess() {
        let l = listing(Some("def"), &[("m.gguf", 1)]);
        assert_eq!(judge(None, &files(&["m.gguf"]), &l, |_| None).0, Verdict::Unknown);
        assert_eq!(judge(Some(""), &files(&["m.gguf"]), &l, |_| None).0, Verdict::Unknown);
        assert_eq!(judge(Some("abc"), &files(&["m.gguf"]), &listing(None, &[]), |_| None).0, Verdict::Unknown);
    }

    #[test]
    fn local_sizes_are_found_next_to_the_model_files() {
        let dir = tempfile::tempdir().expect("dir");
        std::fs::write(dir.path().join("m-00001-of-00002.gguf"), vec![0u8; 7]).expect("write");
        let model = dir.path().join("m-00001-of-00002.gguf");
        assert_eq!(local_size_near(&[model.as_path()], "sub/folder/m-00001-of-00002.gguf"), Some(7));
        assert_eq!(local_size_near(&[model.as_path()], "m-00002-of-00002.gguf"), None);
    }
}
