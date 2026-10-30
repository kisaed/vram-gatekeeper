//! Resumable, verified file downloads (spec §15.3, D9).
//!
//! Responsibility: download one file to `<name>.part`, resume it with an HTTP
//! `Range` request after an interruption, hash it while streaming, and only
//! rename it into place when the size and SHA-256 match.
//! Not responsible for queuing or progress aggregation (`queue`).
//!
//! Implements FR-MOD-4.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::time::Instant;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::gpu::BYTES_PER_MIB;

/// Extra free disk space required beyond the file itself (spec §15.3).
pub const DISK_MARGIN_BYTES: u64 = 1024 * 1024 * 1024;

/// Burst allowance of the rate limiter, so short stalls do not waste
/// bandwidth.
const LIMITER_BURST: Duration = Duration::from_millis(250);

/// Read size when hashing an existing partial file.
const HASH_CHUNK: usize = 1024 * 1024;

/// Why a download did not finish.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DownloadError {
    /// Paused or cancelled by the caller; any `.part` file is left in place.
    #[error("stopped")]
    Stopped,
    /// 401/403.
    #[error("This model needs access. Accept its licence on huggingface.co, then add your access token in Settings.")]
    NeedsAccess,
    /// Not enough disk space.
    #[error("Not enough free disk space: {needed_mib} MiB needed, {available_mib} MiB free.")]
    DiskFull {
        /// MiB required including the safety margin.
        needed_mib: u64,
        /// MiB free.
        available_mib: u64,
    },
    /// The downloaded bytes do not match the published checksum.
    #[error("The downloaded file is damaged (checksum mismatch). It was deleted; download it again.")]
    HashMismatch,
    /// The downloaded size differs from the expected size.
    #[error("The downloaded size is wrong ({got} bytes, expected {expected}).")]
    SizeMismatch {
        /// Bytes on disk.
        got: u64,
        /// Bytes expected.
        expected: u64,
    },
    /// Server error or other HTTP status.
    #[error("The server answered with HTTP {0}.")]
    Http(u16),
    /// Connection problem; resuming usually works.
    #[error("Connection problem: {0}")]
    Network(String),
    /// Local file problem.
    #[error("File error: {0}")]
    Io(String),
}

/// One file to fetch.
#[derive(Debug, Clone)]
pub struct FileJob {
    /// Full URL.
    pub url: String,
    /// Final destination.
    pub dest: PathBuf,
    /// Expected size in bytes.
    pub size: u64,
    /// Expected SHA-256 (hex), if known.
    pub sha256: Option<String>,
    /// Accept a finished file of the right size without hashing it again
    /// (it passed its checksum earlier this session). A transfer still verifies.
    pub trust_existing: bool,
}

/// Token-bucket style limiter shared by concurrent downloads.
pub struct RateLimiter {
    /// Bytes per second; 0 disables limiting.
    bytes_per_sec: u64,
    /// Earliest time the next chunk may be sent.
    next: tokio::sync::Mutex<Instant>,
}

impl RateLimiter {
    /// Creates a limiter for `mbps` megabits per second (0 = unlimited).
    pub fn new(mbps: u32) -> Arc<Self> {
        Arc::new(Self { bytes_per_sec: u64::from(mbps) * 125_000, next: tokio::sync::Mutex::new(Instant::now()) })
    }

    /// Waits long enough that `n` more bytes keep the average under the limit.
    pub async fn throttle(&self, n: usize) {
        if self.bytes_per_sec == 0 {
            return;
        }
        let wait = {
            let mut next = self.next.lock().await;
            let now = Instant::now();
            let start = (*next).max(now.checked_sub(LIMITER_BURST).unwrap_or(now));
            *next = start + Duration::from_secs_f64(n as f64 / self.bytes_per_sec as f64);
            next.checked_duration_since(now + LIMITER_BURST)
        };
        if let Some(w) = wait {
            tokio::time::sleep(w).await;
        }
    }
}

/// Builds `<root>/<author>/<repo>/<relative path>` from remote names, refusing
/// anything that could escape `root` (spec §19).
///
/// Returns `None` for absolute paths, `..`, drive letters, backslashes or
/// empty components.
pub fn safe_dest(root: &Path, repo: &str, relative: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in repo.split('/').chain(relative.split('/')) {
        let bad = part.is_empty()
            || part == "."
            || part == ".."
            || part.contains(['\\', ':', '\0', '*', '?', '"', '<', '>', '|'])
            || part.ends_with(['.', ' ']);
        if bad {
            return None;
        }
        out.push(part);
    }
    // Belt and braces: the result must still be strictly inside root.
    let inside = out.starts_with(root) && out.components().all(|c| !matches!(c, Component::ParentDir));
    inside.then_some(out)
}

/// The `.part` path for a destination.
pub fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(std::ffi::OsString::from).unwrap_or_default();
    name.push(".part");
    dest.with_file_name(name)
}

/// SHA-256 of a file, as lowercase hex.
///
/// # Errors
/// Returns an I/O error message.
pub async fn hash_file(path: &Path) -> Result<String, DownloadError> {
    let mut hasher = Sha256::new();
    hash_into(path, &mut hasher).await?;
    Ok(hex::encode(hasher.finalize()))
}

/// Feeds the whole file at `path` into `hasher`.
async fn hash_into(path: &Path, hasher: &mut Sha256) -> Result<(), DownloadError> {
    let mut file = tokio::fs::File::open(path).await.map_err(|e| DownloadError::Io(e.to_string()))?;
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        let n = file.read(&mut buf).await.map_err(|e| DownloadError::Io(e.to_string()))?;
        if n == 0 {
            return Ok(());
        }
        hasher.update(&buf[..n]);
    }
}

/// Free space check; `None` means "unknown, do not block".
fn free_bytes(dir: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        crate::platform::windows::disk::free_bytes(dir)
    }
    #[cfg(not(windows))]
    {
        let _ = dir;
        None
    }
}

/// Everything `download_file` needs besides the job.
pub struct Context<'a> {
    /// HTTP client.
    pub http: &'a reqwest::Client,
    /// Access token for gated repositories.
    pub token: Option<String>,
    /// Compare the SHA-256 when it is known.
    pub verify: bool,
    /// Shared bandwidth limiter.
    pub limiter: Option<Arc<RateLimiter>>,
    /// Fires to pause or cancel; the `.part` file is kept.
    pub stop: CancellationToken,
}

/// Downloads one file, resuming a partial one. `progress` receives the number
/// of bytes of this file present on disk so far.
///
/// # Errors
/// See [`DownloadError`]. [`DownloadError::Stopped`] leaves the `.part` file so
/// the download can resume.
pub async fn download_file(
    ctx: &Context<'_>,
    job: &FileJob,
    mut progress: impl FnMut(u64) + Send,
) -> Result<(), DownloadError> {
    let io = |e: std::io::Error| DownloadError::Io(e.to_string());
    let part = part_path(&job.dest);
    let dir = job.dest.parent().ok_or_else(|| DownloadError::Io("destination has no folder".into()))?;
    tokio::fs::create_dir_all(dir).await.map_err(io)?;

    // Already downloaded and still valid?
    if let Ok(meta) = tokio::fs::metadata(&job.dest).await
        && meta.len() == job.size
        && (!ctx.verify || job.sha256.is_none() || job.trust_existing || hash_file(&job.dest).await.ok() == job.sha256)
    {
        progress(job.size);
        return Ok(());
    }

    let mut have = tokio::fs::metadata(&part).await.map(|m| m.len()).unwrap_or(0);
    if have > job.size {
        let _ = tokio::fs::remove_file(&part).await;
        have = 0;
    }

    // Disk space for what is still to come (spec §15.3).
    let needed = job.size - have + DISK_MARGIN_BYTES;
    if let Some(free) = free_bytes(dir)
        && free < needed
    {
        return Err(DownloadError::DiskFull {
            needed_mib: needed / BYTES_PER_MIB,
            available_mib: free / BYTES_PER_MIB,
        });
    }

    // The hash must cover the whole file, so on resume hash what we already have.
    let mut hasher = Sha256::new();
    if have > 0 {
        hash_into(&part, &mut hasher).await?;
    }

    if have < job.size {
        have = transfer(ctx, job, &part, have, &mut hasher, &mut progress).await?;
    }
    if have != job.size {
        return Err(DownloadError::SizeMismatch { got: have, expected: job.size });
    }

    if ctx.verify
        && let Some(expected) = &job.sha256
    {
        let actual = hex::encode(hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(DownloadError::HashMismatch);
        }
    }
    // Replaces any older file of the same name atomically on NTFS.
    tokio::fs::rename(&part, &job.dest).await.map_err(io)?;
    progress(job.size);
    Ok(())
}

/// Streams the remainder of the file into `part`. Returns the bytes on disk.
async fn transfer(
    ctx: &Context<'_>,
    job: &FileJob,
    part: &Path,
    mut have: u64,
    hasher: &mut Sha256,
    progress: &mut (impl FnMut(u64) + Send),
) -> Result<u64, DownloadError> {
    let io = |e: std::io::Error| DownloadError::Io(e.to_string());
    let mut req = ctx.http.get(&job.url);
    if let Some(t) = &ctx.token {
        req = req.bearer_auth(t);
    }
    if have > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    let resp = tokio::select! {
        r = req.send() => r.map_err(|e| DownloadError::Network(e.to_string()))?,
        () = ctx.stop.cancelled() => return Err(DownloadError::Stopped),
    };
    match resp.status().as_u16() {
        206 => {}
        200 => {
            // The server ignored Range (or this is a fresh start): start over.
            if have > 0 {
                have = 0;
                *hasher = Sha256::new();
            }
        }
        401 | 403 => return Err(DownloadError::NeedsAccess),
        416 => {
            // Our partial data does not fit the file any more: discard it.
            let _ = tokio::fs::remove_file(part).await;
            return Err(DownloadError::Network("the partial file no longer matched; retrying from the start".into()));
        }
        other => return Err(DownloadError::Http(other)),
    }

    let mut file = if have > 0 {
        tokio::fs::OpenOptions::new().append(true).open(part).await.map_err(io)?
    } else {
        tokio::fs::File::create(part).await.map_err(io)?
    };

    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            chunk = stream.next() => chunk,
            () = ctx.stop.cancelled() => {
                file.flush().await.map_err(io)?;
                return Err(DownloadError::Stopped);
            }
        };
        match next {
            Some(Ok(bytes)) => {
                if have + bytes.len() as u64 > job.size {
                    return Err(DownloadError::SizeMismatch { got: have + bytes.len() as u64, expected: job.size });
                }
                if let Some(l) = &ctx.limiter {
                    l.throttle(bytes.len()).await;
                }
                file.write_all(&bytes).await.map_err(io)?;
                hasher.update(&bytes);
                have += bytes.len() as u64;
                progress(have);
            }
            Some(Err(e)) => {
                file.flush().await.map_err(io)?;
                return Err(DownloadError::Network(e.to_string()));
            }
            None => break,
        }
    }
    file.flush().await.map_err(io)?;
    Ok(have)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_dest_joins_clean_names_under_the_root() {
        let root = Path::new(r"C:\Models");
        let p = safe_dest(root, "Qwen/Qwen3-8B-GGUF", "Q4_K_M/model-00001-of-00002.gguf").expect("safe");
        assert!(p.starts_with(root));
        assert!(p.ends_with("model-00001-of-00002.gguf"));
    }

    #[test]
    fn safe_dest_rejects_anything_that_could_escape() {
        let root = Path::new(r"C:\Models");
        for bad in [
            "../x.gguf",
            "a/../../x.gguf",
            "/etc/x.gguf",
            "C:/x.gguf",
            r"a\b.gguf",
            "a//b.gguf",
            "con:x",
            "x.gguf.",
            "a/./b",
            "",
        ] {
            assert!(safe_dest(root, "o/r", bad).is_none(), "'{bad}' must be refused");
        }
        assert!(safe_dest(root, "../evil/r", "x.gguf").is_none());
        assert!(safe_dest(root, "", "x.gguf").is_none());
    }

    #[test]
    fn part_files_sit_next_to_the_destination() {
        assert_eq!(part_path(Path::new(r"C:\M\a\m.gguf")), PathBuf::from(r"C:\M\a\m.gguf.part"));
    }

    #[tokio::test(start_paused = true)]
    async fn limiter_spreads_bytes_over_time() {
        let l = RateLimiter::new(8); // 1 MB/s
        let start = tokio::time::Instant::now();
        for _ in 0..4 {
            l.throttle(500_000).await; // 2 MB in total
        }
        let took = start.elapsed();
        assert!(took >= Duration::from_millis(1500) && took <= Duration::from_secs(3), "took {took:?}");
        let unlimited = RateLimiter::new(0);
        let t0 = tokio::time::Instant::now();
        unlimited.throttle(100_000_000).await;
        assert_eq!(t0.elapsed(), Duration::ZERO);
    }
}
