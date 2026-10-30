//! In-app engine updates (spec §16.2, Phase 5).
//!
//! Responsibility: list llama.cpp releases, choose the build that matches the
//! user's GPU, download it with checksum verification, and unpack it into
//! `engines\<tag>-<backend>\` under the data folder. The bundled engine is
//! never touched, so it stays the rollback target.
//! Not responsible for: choosing *when* to check. Every call here is started by
//! the user (no telemetry, no background checks).
//!
//! Implements FR-ENG-2.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::error::{CoreError, CoreResult};
use crate::events::{AppEvent, EventBus};
use crate::hf::download::{Context, DownloadError, FileJob, download_file};
use crate::sync::MutexExt;

/// Where releases are listed. `releases/latest` points at a tag without
/// binaries (Phase 0 finding), so the list is used and the newest tag with a
/// matching build wins.
pub const RELEASES_URL: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=30";

/// PCI vendor id of NVIDIA.
const VENDOR_NVIDIA: u32 = 0x10DE;

/// A kind of llama.cpp build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// NVIDIA CUDA.
    Cuda,
    /// Vulkan, which works on NVIDIA, AMD and Intel.
    Vulkan,
    /// No GPU.
    Cpu,
}

impl Backend {
    /// Every backend, in the order alternatives are offered.
    pub const ALL: [Self; 3] = [Self::Vulkan, Self::Cuda, Self::Cpu];

    /// The name used in folder names and release assets.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Cpu => "cpu",
        }
    }
}

/// A file attached to a release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineAsset {
    /// File name, e.g. `llama-b11827-bin-win-vulkan-x64.zip`.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Download address.
    pub url: String,
    /// Lower-case SHA-256 published by GitHub, if any.
    pub sha256: Option<String>,
}

/// One release with its Windows x64 assets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Tag such as `b11827`.
    pub tag: String,
    /// All assets.
    pub assets: Vec<EngineAsset>,
}

/// What to download for one backend of one release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EnginePlan {
    /// Release tag.
    pub tag: String,
    /// Which kind of build.
    pub backend: Backend,
    /// Files to download; the CUDA runtime comes with its build.
    pub assets: Vec<EngineAsset>,
}

impl EnginePlan {
    /// Folder name under `engines\`.
    pub fn folder_name(&self) -> String {
        format!("{}-{}", self.tag, self.backend.as_str())
    }

    /// Total download size in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.assets.iter().map(|a| a.size).sum()
    }
}

/// Result of checking for a newer engine.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateCheck {
    /// Build number of the engine in use, if known.
    pub current_build: Option<u32>,
    /// Build number of the newest release that has a matching build.
    pub latest_build: Option<u32>,
    /// True when the newest release is newer than the one in use.
    pub newer: bool,
    /// What to download for the recommended backend.
    pub plan: Option<EnginePlan>,
    /// The other backends available for the same release (the user may prefer one).
    pub alternatives: Vec<EnginePlan>,
}

/// An engine unpacked under `engines\`.
#[derive(Debug, Clone, Serialize)]
pub struct InstalledEngine {
    /// Folder name, e.g. `b11827-cuda`.
    pub name: String,
    /// Full path of the folder holding `llama-server.exe`.
    pub path: String,
}

/// Progress of a running download, published as an event on every change.
#[derive(Debug, Clone, Default, Serialize)]
pub struct InstallProgress {
    /// What is happening, in words.
    pub step: String,
    /// Bytes downloaded so far.
    pub bytes_done: u64,
    /// Bytes to download in total.
    pub bytes_total: u64,
    /// Set when the install ended (successfully or not).
    pub finished: bool,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// The installed folder, when it worked.
    pub installed: Option<String>,
}

/// Shared install progress. Every change is also published on the event bus
/// (when one was given), so the window never has to poll.
#[derive(Clone, Default)]
pub struct ProgressCell {
    /// Current progress.
    state: Arc<Mutex<InstallProgress>>,
    /// Where changes are announced.
    events: Option<EventBus>,
}

impl ProgressCell {
    /// A cell that announces its changes on `events`.
    pub fn with_events(events: EventBus) -> Self {
        Self { state: Arc::default(), events: Some(events) }
    }

    /// The current progress.
    pub fn get(&self) -> InstallProgress {
        self.state.lock_ok().clone()
    }

    /// Starts a new install unless one is running; returns whether it started.
    pub(crate) fn try_begin(&self, bytes_total: u64) -> bool {
        let started = {
            let mut p = self.state.lock_ok();
            let running = !p.finished && !p.step.is_empty();
            if !running {
                *p = InstallProgress { step: "Starting".into(), bytes_total, ..InstallProgress::default() };
            }
            !running
        };
        if started {
            self.announce();
        }
        started
    }

    /// Records progress and announces it.
    pub(crate) fn update(&self, f: impl FnOnce(&mut InstallProgress)) {
        f(&mut self.state.lock_ok());
        self.announce();
    }

    /// Publishes the current progress.
    fn announce(&self) {
        if let Some(events) = &self.events {
            events.publish(AppEvent::EngineInstall(self.get()));
        }
    }
}

/// The build number in `version: 0.7.0-dev (build 11804, commit abc)` or `b11804`.
pub fn build_number(text: &str) -> Option<u32> {
    let lower = text.to_ascii_lowercase();
    if let Some(i) = lower.find("build ") {
        let digits: String = lower[i + "build ".len()..].chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            return Some(n);
        }
    }
    let tag = lower.trim().strip_prefix('b').unwrap_or(&lower);
    let digits: String = tag.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The backend recommended for an adapter: CUDA for NVIDIA discrete GPUs,
/// otherwise Vulkan, which works on NVIDIA, AMD and Intel.
pub fn recommended_backend(vendor_id: u32, integrated: bool, software: bool) -> Backend {
    if software {
        Backend::Cpu
    } else if vendor_id == VENDOR_NVIDIA && !integrated {
        Backend::Cuda
    } else {
        Backend::Vulkan
    }
}

/// `(major, minor)` from `13.4`.
fn parse_cuda_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Picks the files of `backend` from `release`.
///
/// For CUDA the build and its matching `cudart` zip are both required. When
/// `cuda_hint` (for example the version of the engine in use) is available it
/// is preferred, because a newer CUDA major may need a newer driver; otherwise
/// the highest version wins.
pub fn plan_for(release: &Release, backend: Backend, cuda_hint: Option<(u32, u32)>) -> Option<EnginePlan> {
    let tag = &release.tag;
    let find = |name: &str| release.assets.iter().find(|a| a.name == name).cloned();
    let assets = match backend {
        Backend::Vulkan | Backend::Cpu => {
            vec![find(&format!("llama-{tag}-bin-win-{}-x64.zip", backend.as_str()))?]
        }
        Backend::Cuda => {
            let prefix = format!("llama-{tag}-bin-win-cuda-");
            let mut versions: Vec<(u32, u32)> = release
                .assets
                .iter()
                .filter_map(|a| a.name.strip_prefix(&prefix)?.strip_suffix("-x64.zip").and_then(parse_cuda_version))
                .filter(|v| find(&format!("cudart-llama-bin-win-cuda-{}.{}-x64.zip", v.0, v.1)).is_some())
                .collect();
            versions.sort_unstable();
            let chosen = cuda_hint.filter(|h| versions.contains(h)).or_else(|| versions.last().copied())?;
            vec![
                find(&format!("llama-{tag}-bin-win-cuda-{}.{}-x64.zip", chosen.0, chosen.1))?,
                find(&format!("cudart-llama-bin-win-cuda-{}.{}-x64.zip", chosen.0, chosen.1))?,
            ]
        }
    };
    Some(EnginePlan { tag: tag.clone(), backend, assets })
}

/// The CUDA version in an engine folder name or path such as `...cuda-13.4...`,
/// used as a driver-compatibility hint.
pub fn cuda_hint_from_path(path: &str) -> Option<(u32, u32)> {
    let lower = path.to_ascii_lowercase();
    let i = lower.find("cuda-")? + "cuda-".len();
    let version: String = lower[i..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    parse_cuda_version(version.trim_end_matches('.'))
}

/// Where official engine downloads come from.
const OFFICIAL_DOWNLOAD_PREFIX: &str = "https://github.com/ggml-org/llama.cpp/releases/download/";

/// Refuses plans that do not look like an official llama.cpp release.
///
/// Plans are built from GitHub's release list, which is data from the network:
/// addresses must be the project's release downloads, file and folder names
/// must be plain names, and every file needs a published checksum.
///
/// # Errors
/// Returns an error describing the first problem found.
pub fn check_plan_is_official(plan: &EnginePlan) -> CoreResult<()> {
    let plain = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    };
    if !plain(&plan.tag) || plan.assets.is_empty() {
        return Err(CoreError::Other("That engine download is not valid.".into()));
    }
    for a in &plan.assets {
        if !plain(&a.name) || !a.url.starts_with(OFFICIAL_DOWNLOAD_PREFIX) || a.sha256.is_none() {
            return Err(CoreError::Other(format!(
                "{} is not an official llama.cpp download, so it was not installed.",
                a.name
            )));
        }
    }
    Ok(())
}

/// Raw release JSON as GitHub sends it.
#[derive(Deserialize)]
struct RawRelease {
    /// Tag.
    tag_name: String,
    /// Drafts are never offered.
    #[serde(default)]
    draft: bool,
    /// Assets.
    #[serde(default)]
    assets: Vec<RawAsset>,
}

/// Raw asset JSON.
#[derive(Deserialize)]
struct RawAsset {
    /// File name.
    name: String,
    /// Size.
    size: u64,
    /// Address.
    browser_download_url: String,
    /// `sha256:<hex>`.
    digest: Option<String>,
}

/// Parses the release list.
///
/// # Errors
/// Returns an error if the JSON is not a release list.
pub fn parse_releases(json: &str) -> CoreResult<Vec<Release>> {
    let raw: Vec<RawRelease> =
        serde_json::from_str(json).map_err(|e| CoreError::Other(format!("unreadable release list: {e}")))?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.draft)
        .map(|r| Release {
            tag: r.tag_name,
            assets: r
                .assets
                .into_iter()
                .map(|a| EngineAsset {
                    sha256: a.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
                    name: a.name,
                    size: a.size,
                    url: a.browser_download_url,
                })
                .collect(),
        })
        .collect())
}

/// HTTP client for GitHub (it rejects requests without a user agent).
fn client() -> CoreResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("VRAM-Gatekeeper/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CoreError::Other(format!("http client: {e}")))
}

/// Lists releases from `url`.
///
/// # Errors
/// Returns an error on network failure, a non-success status, or bad JSON.
pub async fn fetch_releases(url: &str) -> CoreResult<Vec<Release>> {
    let response = client()?
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| CoreError::Other(format!("could not reach GitHub: {e}")))?;
    let status = response.status();
    if !status.is_success() {
        return Err(CoreError::Other(if status.as_u16() == 403 {
            "GitHub is limiting requests from this network. Try again in an hour.".to_owned()
        } else {
            format!("GitHub answered {status}")
        }));
    }
    let text = response.text().await.map_err(|e| CoreError::Other(format!("reading the release list: {e}")))?;
    parse_releases(&text)
}

/// Decides what the newest matching build is, given the releases.
pub fn choose_update(
    releases: &[Release],
    backend: Backend,
    current_version: Option<&str>,
    cuda_hint: Option<(u32, u32)>,
) -> UpdateCheck {
    let current_build = current_version.and_then(build_number);
    let newest = releases
        .iter()
        .filter_map(|r| Some((build_number(&r.tag)?, plan_for(r, backend, cuda_hint)?, r)))
        .max_by_key(|(n, _, _)| *n);
    let Some((latest_build, plan, release)) = newest else {
        return UpdateCheck { current_build, latest_build: None, newer: false, plan: None, alternatives: vec![] };
    };
    let alternatives =
        Backend::ALL.into_iter().filter(|b| *b != backend).filter_map(|b| plan_for(release, b, cuda_hint)).collect();
    UpdateCheck {
        current_build,
        latest_build: Some(latest_build),
        newer: current_build.is_none_or(|c| latest_build > c),
        plan: Some(plan),
        alternatives,
    }
}

/// Engines already unpacked under `engines_dir`.
pub fn list_installed(engines_dir: &Path) -> Vec<InstalledEngine> {
    let mut found: Vec<InstalledEngine> = std::fs::read_dir(engines_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("llama-server.exe").is_file())
        .map(|e| InstalledEngine {
            name: e.file_name().to_string_lossy().into_owned(),
            path: e.path().display().to_string(),
        })
        .collect();
    found.sort_by(|a, b| b.name.cmp(&a.name));
    found
}

/// How often download progress is announced.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Downloads `asset` to `dest` with the shared downloader (disk-space check,
/// resume, size and checksum verification). `base` is the number of bytes of
/// the plan's earlier files, so progress covers the whole plan.
async fn download_verified(asset: &EngineAsset, dest: &Path, progress: &ProgressCell, base: u64) -> CoreResult<()> {
    let Some(expected) = asset.sha256.clone() else {
        return Err(CoreError::Other(format!(
            "GitHub published no checksum for {}, so it was not installed.",
            asset.name
        )));
    };
    let http = client()?;
    let ctx = Context { http: &http, token: None, verify: true, limiter: None, stop: CancellationToken::new() };
    let job = FileJob {
        url: asset.url.clone(),
        dest: dest.to_path_buf(),
        size: asset.size,
        sha256: Some(expected),
        trust_existing: false,
    };
    let mut last_announced = Instant::now().checked_sub(PROGRESS_INTERVAL).unwrap_or_else(Instant::now);
    download_file(&ctx, &job, |done| {
        if done == asset.size || last_announced.elapsed() >= PROGRESS_INTERVAL {
            last_announced = Instant::now();
            progress.update(|p| p.bytes_done = base + done);
        }
    })
    .await
    .map_err(|e| match e {
        DownloadError::HashMismatch => {
            CoreError::Other(format!("{} did not match its published checksum. Nothing was installed.", asset.name))
        }
        // The downloader words 401/403 for Hugging Face; from GitHub it usually means rate limiting.
        DownloadError::NeedsAccess => CoreError::Other(format!(
            "GitHub refused the download of {} (it may be limiting requests from this network). Try again in an hour.",
            asset.name
        )),
        other => CoreError::Other(format!("download of {} failed: {other}", asset.name)),
    })
}

/// Unpacks a zip into `dest`, refusing paths that escape it.
fn extract_zip(zip_path: &Path, dest: &Path) -> CoreResult<()> {
    let io = |e: &dyn std::fmt::Display| CoreError::Other(format!("unpacking {}: {e}", zip_path.display()));
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path).map_err(|e| io(&e))?).map_err(|e| io(&e))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| io(&e))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(CoreError::Other(format!(
                "{} contains an unsafe path and was not unpacked.",
                zip_path.display()
            )));
        };
        let out = dest.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| io(&e))?;
        } else {
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
            }
            let mut file = std::fs::File::create(&out).map_err(|e| io(&e))?;
            std::io::copy(&mut entry, &mut file).map_err(|e| io(&e))?;
        }
    }
    Ok(())
}

/// Folder inside `root` that holds `llama-server.exe` (the zip may or may not
/// wrap its files in a folder).
fn find_server_dir(root: &Path) -> Option<PathBuf> {
    if root.join("llama-server.exe").is_file() {
        return Some(root.to_path_buf());
    }
    std::fs::read_dir(root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.is_dir() && p.join("llama-server.exe").is_file())
}

/// Downloads, verifies and unpacks `plan` into `engines_dir\<tag>-<backend>`.
///
/// Nothing appears under the final name until every file verified and unpacked,
/// so an interrupted or failed install leaves no half-engine behind.
///
/// # Errors
/// Returns an error on network, checksum, disk or archive problems.
pub async fn install(plan: &EnginePlan, engines_dir: &Path, progress: &ProgressCell) -> CoreResult<PathBuf> {
    let result = install_inner(plan, engines_dir, progress).await;
    progress.update(|p| {
        p.finished = true;
        match &result {
            Ok(path) => p.installed = Some(path.display().to_string()),
            Err(e) => p.error = Some(e.to_string()),
        }
    });
    result
}

/// The body of [`install`].
async fn install_inner(plan: &EnginePlan, engines_dir: &Path, progress: &ProgressCell) -> CoreResult<PathBuf> {
    let target = engines_dir.join(plan.folder_name());
    if target.join("llama-server.exe").is_file() {
        return Ok(target);
    }
    let work = engines_dir.join(format!(".installing-{}", plan.folder_name()));
    let _ = tokio::fs::remove_dir_all(&work).await;
    let unpack = work.join("unpacked");
    tokio::fs::create_dir_all(&unpack).await.map_err(|e| CoreError::Other(format!("{}: {e}", work.display())))?;
    progress.update(|p| {
        *p = InstallProgress {
            step: "Downloading".into(),
            bytes_total: plan.total_bytes(),
            ..InstallProgress::default()
        };
    });

    let outcome = async {
        let mut zips = Vec::new();
        let mut base = 0;
        for asset in &plan.assets {
            let zip_path = work.join(&asset.name);
            download_verified(asset, &zip_path, progress, base).await?;
            base += asset.size;
            zips.push(zip_path);
        }
        progress.update(|p| "Unpacking".clone_into(&mut p.step));
        let (unpack_for_task, zips_for_task) = (unpack.clone(), zips);
        tokio::task::spawn_blocking(move || zips_for_task.iter().try_for_each(|z| extract_zip(z, &unpack_for_task)))
            .await
            .map_err(|e| CoreError::Other(format!("unpack task failed: {e}")))??;
        let server_dir = find_server_dir(&unpack)
            .ok_or_else(|| CoreError::Other("The download did not contain llama-server.exe.".to_owned()))?;
        tokio::fs::rename(&server_dir, &target)
            .await
            .map_err(|e| CoreError::Other(format!("{}: {e}", target.display())))?;
        Ok::<PathBuf, CoreError>(target.clone())
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    outcome
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    fn asset(name: &str) -> EngineAsset {
        EngineAsset { name: name.into(), size: 10, url: format!("http://x/{name}"), sha256: Some("00".into()) }
    }

    fn release(tag: &str, names: &[&str]) -> Release {
        Release { tag: tag.into(), assets: names.iter().map(|n| asset(n)).collect() }
    }

    fn full(tag: &str) -> Release {
        release(
            tag,
            &[
                &format!("llama-{tag}-bin-win-vulkan-x64.zip"),
                &format!("llama-{tag}-bin-win-cpu-x64.zip"),
                &format!("llama-{tag}-bin-win-cuda-12.4-x64.zip"),
                &format!("llama-{tag}-bin-win-cuda-13.4-x64.zip"),
                "cudart-llama-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-13.4-x64.zip",
                &format!("llama-{tag}-bin-win-cuda-13.4-arm64.zip"),
            ],
        )
    }

    #[test]
    fn build_numbers_come_from_versions_and_tags() {
        assert_eq!(build_number("version: 0.7.0-dev (build 11804, commit c7fc50941)"), Some(11804));
        assert_eq!(build_number("b11827"), Some(11827));
        assert_eq!(build_number("nonsense"), None);
    }

    #[test]
    fn backend_follows_the_gpu() {
        assert_eq!(recommended_backend(0x10DE, false, false), Backend::Cuda);
        assert_eq!(recommended_backend(0x10DE, true, false), Backend::Vulkan);
        assert_eq!(recommended_backend(0x1002, false, false), Backend::Vulkan);
        assert_eq!(recommended_backend(0x1414, false, true), Backend::Cpu);
    }

    #[test]
    fn cuda_plan_pairs_the_build_with_its_runtime_and_respects_the_hint() {
        let r = full("b100");
        let newest = plan_for(&r, Backend::Cuda, None).expect("plan");
        assert_eq!(newest.assets[0].name, "llama-b100-bin-win-cuda-13.4-x64.zip");
        assert_eq!(newest.assets[1].name, "cudart-llama-bin-win-cuda-13.4-x64.zip");
        let hinted = plan_for(&r, Backend::Cuda, Some((12, 4))).expect("plan");
        assert_eq!(hinted.assets[0].name, "llama-b100-bin-win-cuda-12.4-x64.zip");
        assert_eq!(hinted.assets[1].name, "cudart-llama-bin-win-cuda-12.4-x64.zip");
        assert_eq!(hinted.folder_name(), "b100-cuda");
    }

    #[test]
    fn a_cuda_build_without_its_runtime_is_not_offered() {
        let r = release("b1", &["llama-b1-bin-win-cuda-13.4-x64.zip"]);
        assert!(plan_for(&r, Backend::Cuda, None).is_none());
        assert!(plan_for(&r, Backend::Vulkan, None).is_none());
    }

    #[test]
    fn newest_release_with_a_matching_build_wins_and_alternatives_are_listed() {
        let releases = vec![full("b100"), release("b300", &["llama-b300-bin-win-sycl-x64.zip"]), full("b200")];
        let check = choose_update(&releases, Backend::Vulkan, Some("version: 1 (build 150, commit x)"), None);
        assert_eq!(check.latest_build, Some(200), "b300 has no vulkan build");
        assert!(check.newer);
        assert_eq!(check.plan.expect("plan").tag, "b200");
        let alt: Vec<Backend> = check.alternatives.iter().map(|p| p.backend).collect();
        assert_eq!(alt, vec![Backend::Cuda, Backend::Cpu]);
        let same = choose_update(&releases, Backend::Vulkan, Some("build 200"), None);
        assert!(!same.newer);
    }

    #[test]
    fn real_release_json_parses_with_digests() {
        let json = r#"[{"tag_name":"b9","draft":false,"assets":[{"name":"a.zip","size":5,"browser_download_url":"http://h/a.zip","digest":"sha256:ABCDEF"},{"name":"b.zip","size":1,"browser_download_url":"http://h/b.zip","digest":null}]},{"tag_name":"b8","draft":true,"assets":[]}]"#;
        let releases = parse_releases(json).expect("parse");
        assert_eq!(releases.len(), 1, "drafts are skipped");
        assert_eq!(releases[0].assets[0].sha256.as_deref(), Some("abcdef"));
        assert_eq!(releases[0].assets[1].sha256, None);
    }

    #[test]
    fn cuda_hint_is_read_from_names() {
        assert_eq!(cuda_hint_from_path(r"C:\x\llama-b1-bin-win-cuda-13.4-x64"), Some((13, 4)));
        assert_eq!(cuda_hint_from_path("b100-cuda"), None);
    }

    #[test]
    fn only_official_checksummed_downloads_are_accepted() {
        let good = |name: &str, url: &str, sha: Option<&str>| EnginePlan {
            tag: "b9".into(),
            backend: Backend::Vulkan,
            assets: vec![EngineAsset { name: name.into(), size: 1, url: url.into(), sha256: sha.map(str::to_owned) }],
        };
        let ok = "https://github.com/ggml-org/llama.cpp/releases/download/b9/llama-b9-bin-win-vulkan-x64.zip";
        assert!(check_plan_is_official(&good("llama-b9-bin-win-vulkan-x64.zip", ok, Some("ab"))).is_ok());
        assert!(
            check_plan_is_official(&good("llama-b9-bin-win-vulkan-x64.zip", "https://evil.example/x.zip", Some("ab")))
                .is_err()
        );
        assert!(check_plan_is_official(&good("llama-b9-bin-win-vulkan-x64.zip", ok, None)).is_err());
        assert!(check_plan_is_official(&good("a\\b.zip", ok, Some("ab"))).is_err(), "no path separators");
        assert!(check_plan_is_official(&good("a/b.zip", ok, Some("ab"))).is_err());
        let mut bad_tag = good("a.zip", ok, Some("ab"));
        bad_tag.tag = "..".into();
        assert!(check_plan_is_official(&bad_tag).is_err(), "a tag of dots is not a folder name");
    }

    // ---- installing, against a local stub server

    use axum::Router;
    use axum::routing::get;

    /// A zip with `files` (name, content).
    fn make_zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = zip::write::SimpleFileOptions::default();
            for (name, content) in files {
                zip.start_file(*name, options).expect("start");
                std::io::Write::write_all(&mut zip, content.as_bytes()).expect("write");
            }
            zip.finish().expect("finish");
        }
        buf.into_inner()
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    async fn serve(files: Vec<(&'static str, Vec<u8>)>) -> String {
        let mut app = Router::new();
        for (name, data) in files {
            app = app.route(&format!("/{name}"), get(move || async move { data.clone() }));
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
        format!("http://{addr}")
    }

    fn plan_with(base: &str, name: &str, data: &[u8], digest: Option<String>) -> EnginePlan {
        EnginePlan {
            tag: "b9".into(),
            backend: Backend::Vulkan,
            assets: vec![EngineAsset {
                name: name.into(),
                size: data.len() as u64,
                url: format!("{base}/{name}"),
                sha256: digest,
            }],
        }
    }

    #[tokio::test]
    async fn install_verifies_unpacks_and_leaves_no_scratch_files() {
        let zip = make_zip(&[("llama-server.exe", "exe"), ("ggml.dll", "dll")]);
        let base = serve(vec![("e.zip", zip.clone())]).await;
        let dir = tempfile::tempdir().expect("dir");
        let progress = ProgressCell::default();
        let plan = plan_with(&base, "e.zip", &zip, Some(sha(&zip)));
        let path = install(&plan, dir.path(), &progress).await.expect("install");
        assert_eq!(path, dir.path().join("b9-vulkan"));
        assert!(path.join("llama-server.exe").is_file() && path.join("ggml.dll").is_file());
        assert_eq!(std::fs::read_dir(dir.path()).expect("read").count(), 1, "scratch folder removed");
        let p = progress.get();
        assert!(p.finished && p.error.is_none() && p.bytes_done == zip.len() as u64, "{p:?}");
        assert_eq!(list_installed(dir.path()).len(), 1);
        // A second install is a no-op that returns the same folder.
        assert_eq!(install(&plan, dir.path(), &progress).await.expect("again"), path);
    }

    #[tokio::test]
    async fn a_zip_that_wraps_its_files_in_a_folder_is_flattened() {
        let zip = make_zip(&[("llama-b9/llama-server.exe", "exe"), ("llama-b9/x.dll", "dll")]);
        let base = serve(vec![("w.zip", zip.clone())]).await;
        let dir = tempfile::tempdir().expect("dir");
        let plan = plan_with(&base, "w.zip", &zip, Some(sha(&zip)));
        let path = install(&plan, dir.path(), &ProgressCell::default()).await.expect("install");
        assert!(path.join("llama-server.exe").is_file() && path.join("x.dll").is_file());
    }

    #[tokio::test]
    async fn a_checksum_mismatch_installs_nothing() {
        let zip = make_zip(&[("llama-server.exe", "exe")]);
        let base = serve(vec![("e.zip", zip.clone())]).await;
        let dir = tempfile::tempdir().expect("dir");
        let progress = ProgressCell::default();
        let plan = plan_with(&base, "e.zip", &zip, Some("00".repeat(32)));
        let err = install(&plan, dir.path(), &progress).await.expect_err("must fail").to_string();
        assert!(err.contains("checksum"), "{err}");
        assert_eq!(std::fs::read_dir(dir.path()).expect("read").count(), 0, "no half-installed engine");
        assert!(progress.get().error.is_some());
    }

    #[tokio::test]
    async fn a_release_without_a_published_checksum_is_refused() {
        let zip = make_zip(&[("llama-server.exe", "exe")]);
        let base = serve(vec![("e.zip", zip.clone())]).await;
        let dir = tempfile::tempdir().expect("dir");
        let plan = plan_with(&base, "e.zip", &zip, None);
        let err = install(&plan, dir.path(), &ProgressCell::default()).await.expect_err("must fail").to_string();
        assert!(err.contains("no checksum"), "{err}");
    }

    #[tokio::test]
    async fn an_archive_with_a_path_escape_is_refused() {
        let zip = make_zip(&[("../evil.dll", "x"), ("llama-server.exe", "exe")]);
        let base = serve(vec![("e.zip", zip.clone())]).await;
        let dir = tempfile::tempdir().expect("dir");
        let plan = plan_with(&base, "e.zip", &zip, Some(sha(&zip)));
        let err = install(&plan, dir.path(), &ProgressCell::default()).await.expect_err("must fail").to_string();
        assert!(err.contains("unsafe path"), "{err}");
        assert!(!dir.path().join("evil.dll").exists());
    }

    #[tokio::test]
    async fn fetching_the_release_list_reads_the_stub() {
        let json = r#"[{"tag_name":"b9","assets":[]}]"#.as_bytes().to_vec();
        let base = serve(vec![("releases", json)]).await;
        let releases = fetch_releases(&format!("{base}/releases")).await.expect("fetch");
        assert_eq!(releases[0].tag, "b9");
        assert!(fetch_releases(&format!("{base}/missing")).await.is_err());
    }
}
