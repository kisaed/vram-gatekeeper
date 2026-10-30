//! Finding draft models for a repository (spec §10.6 "Find Draft Models").
//!
//! Responsibility: turn a repository name into the searches that surface draft
//! models of the same family, and merge their results into one short list.
//! Not responsible for the Hub requests themselves (`api`): the caller passes
//! the search function, which keeps this module testable without a network.

use std::future::Future;

use serde::Serialize;

use super::api::{HfError, RepoSummary};

/// Results kept after merging.
pub const MAX_SUGGESTIONS: usize = 20;

/// Words added to the family name to find draft-style repositories.
const DRAFT_WORDS: [&str; 3] = ["eagle3", "draft", "speculative"];

/// A repository that may serve as a draft model, and why it was suggested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DraftSuggestion {
    /// The repository.
    pub repo: RepoSummary,
    /// Why it was suggested.
    pub reason: String,
}

/// True for a size token such as `8B`, `0.5B` or `270M`.
fn parse_size_token(token: &str) -> Option<f64> {
    let unit = token.chars().last()?.to_ascii_lowercase();
    let number = token.get(..token.len() - 1)?;
    if !number.chars().next()?.is_ascii_digit() || !number.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let value: f64 = number.parse().ok()?;
    match unit {
        'b' => Some(value),
        'm' => Some(value / 1000.0),
        _ => None,
    }
}

/// The repository name without its owner.
fn name_of(repo_id: &str) -> &str {
    repo_id.rsplit('/').next().unwrap_or(repo_id)
}

/// Splits a name at `-` and `_`.
fn tokens(name: &str) -> Vec<&str> {
    name.split(['-', '_']).filter(|t| !t.is_empty()).collect()
}

/// The size in billions of parameters named in the repository, if any
/// (`Qwen/Qwen3-8B-GGUF` is 8, `x/Model-0.5B-GGUF` is 0.5).
pub fn size_token(repo_id: &str) -> Option<f64> {
    tokens(name_of(repo_id)).into_iter().find_map(parse_size_token)
}

/// The model family: the repository name without its owner, without a
/// trailing `-GGUF` or `_GGUF`, and without the first size token. Words are
/// joined with `-`.
pub fn family_of(repo_id: &str) -> String {
    let mut words = tokens(name_of(repo_id));
    if words.last().is_some_and(|w| w.eq_ignore_ascii_case("gguf")) {
        words.pop();
    }
    if let Some(at) = words.iter().position(|w| parse_size_token(w).is_some()) {
        words.remove(at);
    }
    words.join("-")
}

/// Searches for draft models of `repo`'s family with `search` and merges the
/// results: smaller models of the same family first, then repositories whose
/// names say draft. The repository itself is never suggested.
///
/// # Errors
/// Returns the first search error.
pub async fn find_draft_models<F, Fut>(repo: &str, search: F) -> Result<Vec<DraftSuggestion>, HfError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<RepoSummary>, HfError>>,
{
    let family = family_of(repo);
    if family.is_empty() {
        return Ok(Vec::new());
    }
    let target_size = size_token(repo);
    let mut out: Vec<DraftSuggestion> = Vec::new();
    let mut add = |repo_summary: RepoSummary, reason: String| {
        let taken = repo_summary.id.eq_ignore_ascii_case(repo)
            || out.iter().any(|s| s.repo.id.eq_ignore_ascii_case(&repo_summary.id));
        if !taken && out.len() < MAX_SUGGESTIONS {
            out.push(DraftSuggestion { repo: repo_summary, reason });
        }
    };

    for found in search(family.clone()).await? {
        let smaller = size_token(&found.id).is_some_and(|s| target_size.is_none_or(|t| s < t));
        if smaller {
            add(found, "Smaller model of the same family.".to_owned());
        }
    }
    for word in DRAFT_WORDS {
        for found in search(format!("{family} {word}")).await? {
            add(found, format!("The name mentions {word}."));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn summary(id: &str) -> RepoSummary {
        RepoSummary {
            id: id.into(),
            author: id.split('/').next().unwrap_or_default().into(),
            downloads: 1,
            likes: 1,
            last_modified: None,
            gated: false,
        }
    }

    #[test]
    fn family_strips_owner_gguf_suffix_and_size() {
        assert_eq!(family_of("Qwen/Qwen3-8B-GGUF"), "Qwen3");
        assert_eq!(family_of("x/Llama-3.2-1B-Instruct-GGUF"), "Llama-3.2-Instruct");
        assert_eq!(family_of("x/gemma_2_9b_it_GGUF"), "gemma-2-it");
        assert_eq!(family_of("x/NoSizeHere"), "NoSizeHere");
        assert_eq!(family_of("x/Qwen3-30B-A3B-GGUF"), "Qwen3-A3B", "only the first size token goes");
    }

    #[test]
    fn size_token_orders_0_5b_below_8b() {
        assert_eq!(size_token("Qwen/Qwen3-8B-GGUF"), Some(8.0));
        assert_eq!(size_token("Qwen/Qwen3-0.6B-GGUF"), Some(0.6));
        assert_eq!(size_token("g/gemma-3-270m-it"), Some(0.27));
        assert_eq!(size_token("x/Llama-3.2-Instruct"), None, "3.2 has no unit");
        assert!(size_token("a/M-0.5B").expect("size") < size_token("a/M-8B").expect("size"));
    }

    #[tokio::test]
    async fn find_draft_models_merges_dedupes_and_excludes_the_target() {
        let queries = Mutex::new(Vec::<String>::new());
        let found = find_draft_models("Qwen/Qwen3-8B-GGUF", |q| {
            queries.lock().expect("lock").push(q.clone());
            let result = match q.as_str() {
                "Qwen3" => {
                    vec![summary("Qwen/Qwen3-8B-GGUF"), summary("Qwen/Qwen3-0.6B-GGUF"), summary("Qwen/Qwen3-32B-GGUF")]
                }
                "Qwen3 eagle3" => vec![summary("yuhuili/Qwen3-8B-EAGLE3"), summary("Qwen/Qwen3-0.6B-GGUF")],
                "Qwen3 draft" => vec![summary("someone/Qwen3-draft-GGUF")],
                _ => vec![],
            };
            async move { Ok(result) }
        })
        .await
        .expect("search");
        assert_eq!(*queries.lock().expect("lock"), ["Qwen3", "Qwen3 eagle3", "Qwen3 draft", "Qwen3 speculative"]);
        let ids: Vec<&str> = found.iter().map(|s| s.repo.id.as_str()).collect();
        assert_eq!(ids, ["Qwen/Qwen3-0.6B-GGUF", "yuhuili/Qwen3-8B-EAGLE3", "someone/Qwen3-draft-GGUF"]);
        assert_eq!(found[0].reason, "Smaller model of the same family.");
    }

    #[tokio::test]
    async fn a_search_error_is_returned() {
        let result = find_draft_models("a/B-1B", |_| async { Err(HfError::Parse("boom".into())) }).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn at_most_the_limit_is_returned() {
        let many: Vec<RepoSummary> = (0..40).map(|i| summary(&format!("o/draft-{i}"))).collect();
        let found = find_draft_models("a/B-8B", |_| {
            let many = many.clone();
            async move { Ok(many) }
        })
        .await
        .expect("search");
        assert_eq!(found.len(), MAX_SUGGESTIONS);
    }
}
