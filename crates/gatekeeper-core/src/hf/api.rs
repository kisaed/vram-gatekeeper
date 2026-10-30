//! Hugging Face Hub API client (spec §15.1, §15.2, F13).
//!
//! Responsibility: search for GGUF repositories, list a repository's files
//! (with their SHA-256) and build download URLs, with retries, rate-limit
//! handling and an in-memory search cache.
//! Not responsible for: grouping files (`gguf_files`) or downloading them
//! (`download`).
//!
//! The base URL is a parameter so tests can point it at a local stub.
//!
//! Implements FR-MOD-4.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sync::MutexExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The real Hub.
pub const HUB_URL: &str = "https://huggingface.co";

/// Searches are cached for this long (spec §15.1).
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(5 * 60);

/// Attempts for a request that fails with a transient error.
const MAX_ATTEMPTS: u32 = 3;

/// Pages of a tree listing we will follow before giving up.
const MAX_PAGES: usize = 30;

/// A file in a repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoFile {
    /// Path inside the repository, always with `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the content, known for LFS files (which all GGUFs are).
    pub sha256: Option<String>,
}

/// A repository in search results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoSummary {
    /// `author/name`.
    pub id: String,
    /// The author part of the id.
    pub author: String,
    /// Download count.
    pub downloads: u64,
    /// Like count.
    pub likes: u64,
    /// Last modification time as sent by the Hub.
    pub last_modified: Option<String>,
    /// True for gated repositories (licence must be accepted).
    pub gated: bool,
}

/// A repository's files at one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoListing {
    /// Commit the files belong to, when the Hub told us.
    pub sha: Option<String>,
    /// All files.
    pub files: Vec<RepoFile>,
}

/// How search results are ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    /// Most downloaded first.
    Downloads,
    /// Most liked first.
    Likes,
    /// Most recently updated first.
    Updated,
}

impl SearchSort {
    /// The `sort` query value.
    fn param(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::Updated => "lastModified",
        }
    }
}

/// Why a Hub request failed, in terms a person can act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HfError {
    /// 401/403: gated or private repository, or a bad token.
    #[error("This model needs access. Accept its licence on huggingface.co, then add your access token in Settings.")]
    NeedsAccess,
    /// 404.
    #[error("Not found on Hugging Face.")]
    NotFound,
    /// 429, with the suggested wait.
    #[error("Hugging Face is rate limiting requests. Try again in {0} seconds.")]
    RateLimited(u64),
    /// Any other HTTP error.
    #[error("Hugging Face answered with HTTP {0}.")]
    Http(u16),
    /// No connection or a broken one.
    #[error("Could not reach Hugging Face: {0}")]
    Network(String),
    /// The reply was not what we expected.
    #[error("Unexpected reply from Hugging Face: {0}")]
    Parse(String),
}

/// Source of the access token, read per request so a newly saved token applies
/// at once.
pub type TokenSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Cached search results: `(query, sort) -> (when, results)`.
type SearchCache = Arc<Mutex<HashMap<(String, SearchSort), (Instant, Vec<RepoSummary>)>>>;

/// The client.
#[derive(Clone)]
pub struct HfApi {
    /// Shared HTTP client.
    http: reqwest::Client,
    /// Base URL without a trailing slash.
    base: String,
    /// Access token provider.
    token: TokenSource,
    /// Recent searches.
    cache: SearchCache,
}

/// Percent-encodes a path, keeping `/` and unreserved characters.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl HfApi {
    /// Creates a client for `base`.
    ///
    /// # Errors
    /// Returns an error if the HTTP client cannot be built.
    pub fn new(base: impl Into<String>, token: TokenSource) -> Result<Self, HfError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("VRAMGatekeeper/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| HfError::Network(e.to_string()))?;
        Ok(Self { http, base: base.into().trim_end_matches('/').to_owned(), token, cache: Arc::default() })
    }

    /// The shared HTTP client (the downloader reuses its connection pool).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The current access token, if one is saved.
    pub fn token(&self) -> Option<String> {
        (self.token)()
    }

    /// URL for downloading `path` of `repo` at `revision`.
    pub fn resolve_url(&self, repo: &str, revision: &str, path: &str) -> String {
        format!("{}/{}/resolve/{}/{}", self.base, encode_path(repo), encode_path(revision), encode_path(path))
    }

    /// Sends a GET with retries for transient failures. Returns the response
    /// only for success statuses.
    async fn get(&self, url: &str) -> Result<reqwest::Response, HfError> {
        let mut delay = Duration::from_secs(1);
        let mut last = HfError::Network("no attempt was made".into());
        for attempt in 1..=MAX_ATTEMPTS {
            let mut req = self.http.get(url);
            if let Some(t) = self.token() {
                req = req.bearer_auth(t);
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    match status {
                        200..=299 => return Ok(resp),
                        401 | 403 => return Err(HfError::NeedsAccess),
                        404 => return Err(HfError::NotFound),
                        429 => {
                            let wait = resp
                                .headers()
                                .get("retry-after")
                                .and_then(|v| v.to_str().ok())
                                .and_then(|v| v.parse::<u64>().ok())
                                .unwrap_or(delay.as_secs());
                            last = HfError::RateLimited(wait);
                            if attempt < MAX_ATTEMPTS && wait <= 10 {
                                tokio::time::sleep(Duration::from_secs(wait)).await;
                                continue;
                            }
                            return Err(last);
                        }
                        500..=599 => last = HfError::Http(status),
                        other => return Err(HfError::Http(other)),
                    }
                }
                Err(e) => last = HfError::Network(e.to_string()),
            }
            if attempt < MAX_ATTEMPTS {
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
        Err(last)
    }

    /// Searches for GGUF repositories.
    ///
    /// # Errors
    /// Returns an [`HfError`] describing what went wrong.
    pub async fn search(&self, query: &str, sort: SearchSort, limit: u32) -> Result<Vec<RepoSummary>, HfError> {
        let key = (query.trim().to_lowercase(), sort);
        if let Some((at, hit)) = self.cache.lock_ok().get(&key)
            && at.elapsed() < SEARCH_CACHE_TTL
        {
            return Ok(hit.clone());
        }
        let url = format!(
            "{}/api/models?search={}&filter=gguf&sort={}&direction=-1&limit={limit}",
            self.base,
            encode_path(query.trim()),
            sort.param()
        );
        let body: Value = self.get(&url).await?.json().await.map_err(|e| HfError::Parse(e.to_string()))?;
        let items = body.as_array().ok_or_else(|| HfError::Parse("search did not return a list".into()))?;
        let results: Vec<RepoSummary> = items
            .iter()
            .filter_map(|m| {
                let id = m.get("id").or_else(|| m.get("modelId"))?.as_str()?.to_owned();
                Some(RepoSummary {
                    author: id.split('/').next().unwrap_or_default().to_owned(),
                    downloads: m.get("downloads").and_then(Value::as_u64).unwrap_or(0),
                    likes: m.get("likes").and_then(Value::as_u64).unwrap_or(0),
                    last_modified: m.get("lastModified").and_then(Value::as_str).map(str::to_owned),
                    // `gated` is `false`, or the string "auto" / "manual".
                    gated: m.get("gated").is_some_and(|g| g.as_bool().unwrap_or(true)),
                    id,
                })
            })
            .collect();
        self.cache.lock_ok().insert(key, (Instant::now(), results.clone()));
        Ok(results)
    }

    /// Lists every file of `repo` at `revision`, following pagination.
    ///
    /// # Errors
    /// Returns an [`HfError`].
    pub async fn listing(&self, repo: &str, revision: &str) -> Result<RepoListing, HfError> {
        let mut url =
            format!("{}/api/models/{}/tree/{}?recursive=true", self.base, encode_path(repo), encode_path(revision));
        let mut files = Vec::new();
        for _ in 0..MAX_PAGES {
            let resp = self.get(&url).await?;
            let next = next_link(resp.headers());
            let page: Value = resp.json().await.map_err(|e| HfError::Parse(e.to_string()))?;
            let entries = page.as_array().ok_or_else(|| HfError::Parse("tree did not return a list".into()))?;
            files.extend(entries.iter().filter_map(parse_tree_entry));
            match next {
                Some(n) => url = n,
                None => break,
            }
        }
        // The commit sha is a convenience; failing to get it is not an error.
        let sha_url = format!("{}/api/models/{}/revision/{}", self.base, encode_path(repo), encode_path(revision));
        let sha = match self.get(&sha_url).await {
            Ok(r) => r.json::<Value>().await.ok().and_then(|v| v.get("sha").and_then(Value::as_str).map(str::to_owned)),
            Err(_) => None,
        };
        Ok(RepoListing { sha, files })
    }
}

/// One tree entry as a file, if it is one.
fn parse_tree_entry(e: &Value) -> Option<RepoFile> {
    if e.get("type").and_then(Value::as_str) != Some("file") {
        return None;
    }
    Some(RepoFile {
        path: e.get("path")?.as_str()?.to_owned(),
        size: e
            .get("lfs")
            .and_then(|l| l.get("size"))
            .and_then(Value::as_u64)
            .or_else(|| e.get("size").and_then(Value::as_u64))?,
        sha256: e.get("lfs").and_then(|l| l.get("oid")).and_then(Value::as_str).map(str::to_owned),
    })
}

/// Extracts the `rel="next"` URL from a `Link` header.
fn next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get(reqwest::header::LINK)?.to_str().ok()?;
    link.split(',').find(|part| part.contains("rel=\"next\"")).and_then(|part| {
        let start = part.find('<')? + 1;
        let end = part.find('>')?;
        Some(part[start..end].to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_percent_encoded_but_keep_slashes() {
        assert_eq!(encode_path("Qwen/Qwen3 8B/file name+x.gguf"), "Qwen/Qwen3%208B/file%20name%2Bx.gguf");
        assert_eq!(encode_path("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn link_header_next_is_found() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            "<https://x/api/page2?cursor=abc>; rel=\"next\", <https://x/p0>; rel=\"prev\"".parse().expect("header"),
        );
        assert_eq!(next_link(&h).as_deref(), Some("https://x/api/page2?cursor=abc"));
        assert_eq!(next_link(&reqwest::header::HeaderMap::new()), None);
    }

    #[test]
    fn tree_entries_use_lfs_size_and_hash() {
        let lfs = serde_json::json!({"type":"file","path":"a/m.gguf","size":134,"lfs":{"oid":"abc","size":4000000,"pointerSize":134}});
        let f = parse_tree_entry(&lfs).expect("file");
        assert_eq!((f.size, f.sha256.as_deref()), (4_000_000, Some("abc")));
        let plain = serde_json::json!({"type":"file","path":"README.md","size":12,"oid":"gitsha"});
        let f = parse_tree_entry(&plain).expect("file");
        assert_eq!((f.size, f.sha256), (12, None));
        assert!(parse_tree_entry(&serde_json::json!({"type":"directory","path":"d"})).is_none());
    }
}
