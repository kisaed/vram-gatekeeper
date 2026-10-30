//! Tests for the Hugging Face client, downloader and queue against a local stub
//! of the Hub (spec §22.2). The stub speaks the parts of the real API we use:
//! search, paginated tree, commit sha, and `resolve` with `Range`, redirects,
//! gated access, dropped connections and corrupt bytes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as AxPath, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bytes::Bytes;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::api::{HfApi, HfError, RepoFile, SearchSort};
use super::download::{Context, DownloadError, FileJob, download_file, part_path};
use super::queue::{DownloadManager, DownloadSpec, DownloadState};
use crate::config::schema::Config;
use crate::events::{AppEvent, EventBus};

/// Size of the test file: several chunks, small enough to be quick.
const FILE_LEN: usize = 3 * 1024 * 1024 + 123;

/// Deterministic pseudo-random content.
pub(crate) fn content(seed: u8) -> Vec<u8> {
    let mut state = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..FILE_LEN)
        .map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (state >> 24) as u8
        })
        .collect()
}

pub(crate) fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// How the stub misbehaves.
#[derive(Default, Clone)]
struct Behavior {
    /// Cut the connection after this many bytes on the first full request only.
    drop_after: Option<usize>,
    /// Answer 200 with the whole file even when a Range was requested.
    ignore_range: bool,
    /// Flip a byte so the checksum no longer matches.
    corrupt: bool,
    /// Require `Authorization: Bearer secret`.
    gated: bool,
    /// Slow the stream so a pause can land mid-transfer.
    slow: bool,
}

pub(crate) struct Stub {
    files: HashMap<String, Vec<u8>>,
    behavior: Mutex<Behavior>,
    dropped_once: Mutex<bool>,
    /// The stub's own address, so pagination links can be absolute.
    base: Mutex<String>,
    searches: AtomicUsize,
    range_requests: AtomicUsize,
    full_requests: AtomicUsize,
}

pub(crate) type Shared = Arc<Stub>;

async fn search(State(s): State<Shared>) -> impl IntoResponse {
    s.searches.fetch_add(1, Ordering::SeqCst);
    axum::Json(json!([
        { "id": "org/Model-GGUF", "downloads": 1234, "likes": 7, "lastModified": "2026-01-01T00:00:00Z", "gated": false },
        { "id": "other/Gated-GGUF", "downloads": 5, "likes": 1, "gated": "manual" }
    ]))
}

async fn tree(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
    AxPath((org, name, _rev)): AxPath<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if s.behavior.lock().expect("lock").gated && headers.get(header::AUTHORIZATION).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if format!("{org}/{name}") != "org/Model-GGUF" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut names: Vec<&String> = s.files.keys().collect();
    names.sort();
    let entry = |n: &&String| {
        let data = &s.files[*n];
        json!({ "type": "file", "path": n, "size": 130, "lfs": { "oid": sha(data), "size": data.len(), "pointerSize": 130 } })
    };
    // Page two is reached through the Link header, as on the real Hub.
    if q.contains_key("cursor") {
        let page: Vec<_> =
            names.iter().skip(1).map(entry).chain([json!({ "type": "directory", "path": "sub" })]).collect();
        return axum::Json(page).into_response();
    }
    let page: Vec<_> = names
        .iter()
        .take(1)
        .map(entry)
        .chain([json!({ "type": "file", "path": "README.md", "size": 9, "oid": "gitsha" })])
        .collect();
    let mut resp = axum::Json(page).into_response();
    let next = format!(
        "<{}/api/models/org/Model-GGUF/tree/main?recursive=true&cursor=2>; rel=\"next\"",
        s.base.lock().expect("lock")
    );
    resp.headers_mut().insert(header::LINK, HeaderValue::from_str(&next).expect("header"));
    resp
}

async fn revision() -> impl IntoResponse {
    axum::Json(json!({ "sha": "deadbeefcafe" }))
}

async fn resolve(
    State(s): State<Shared>,
    AxPath((_org, _name, _rev, path)): AxPath<(String, String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let b = s.behavior.lock().expect("lock").clone();
    if b.gated && headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Bearer secret") {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(data) = s.files.get(&path) else { return StatusCode::NOT_FOUND.into_response() };
    let mut data = data.clone();
    if b.corrupt {
        data[1000] ^= 0xFF;
    }

    let range_start = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| v.strip_suffix('-'))
        .and_then(|v| v.parse::<usize>().ok());
    let (status, start) = match range_start {
        Some(start) if !b.ignore_range => {
            s.range_requests.fetch_add(1, Ordering::SeqCst);
            if start >= data.len() {
                return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
            }
            (StatusCode::PARTIAL_CONTENT, start)
        }
        _ => {
            s.full_requests.fetch_add(1, Ordering::SeqCst);
            (StatusCode::OK, 0)
        }
    };
    let body_bytes = data[start..].to_vec();
    let total = body_bytes.len();

    // Cut the connection once, on the first full download, if asked.
    let cut = {
        let mut dropped = s.dropped_once.lock().expect("lock");
        match (b.drop_after, *dropped, status) {
            (Some(n), false, StatusCode::OK) => {
                *dropped = true;
                Some(n)
            }
            _ => None,
        }
    };
    let slow = b.slow;
    let stream = futures_util::stream::unfold(0usize, move |sent| {
        let body_bytes = body_bytes.clone();
        async move {
            if sent >= body_bytes.len() {
                return None;
            }
            if cut.is_some_and(|c| sent >= c) {
                return Some((Err(std::io::Error::other("connection reset")), body_bytes.len()));
            }
            if slow {
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            let end = (sent + 64 * 1024).min(body_bytes.len());
            Some((Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&body_bytes[sent..end])), end))
        }
    });
    let mut resp = Response::new(Body::from_stream(stream));
    *resp.status_mut() = status;
    resp.headers_mut().insert(header::CONTENT_LENGTH, HeaderValue::from(total));
    resp
}

/// Starts the stub; `files` are the repo's files.
pub(crate) async fn start_stub(files: Vec<(&str, Vec<u8>)>) -> (String, Shared) {
    let stub = Arc::new(Stub {
        files: files.into_iter().map(|(n, d)| (n.to_owned(), d)).collect(),
        behavior: Mutex::new(Behavior::default()),
        dropped_once: Mutex::new(false),
        base: Mutex::new(String::new()),
        searches: AtomicUsize::new(0),
        range_requests: AtomicUsize::new(0),
        full_requests: AtomicUsize::new(0),
    });
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    *stub.base.lock().expect("lock") = base.clone();
    let app = Router::new()
        .route("/api/models", get(search))
        .route("/api/models/{org}/{name}/tree/{rev}", get(tree))
        .route("/api/models/{org}/{name}/revision/{rev}", get(revision))
        .route("/{org}/{name}/resolve/{rev}/{*path}", get(resolve))
        .with_state(stub.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, stub)
}

fn api(base: &str, token: Option<&str>) -> HfApi {
    let token = token.map(str::to_owned);
    HfApi::new(base, Arc::new(move || token.clone())).expect("client")
}

fn job(api: &HfApi, dest: std::path::PathBuf, data: &[u8]) -> FileJob {
    FileJob {
        url: api.resolve_url("org/Model-GGUF", "main", "m-Q4_K_M.gguf"),
        dest,
        size: data.len() as u64,
        sha256: Some(sha(data)),
        trust_existing: false,
    }
}

fn ctx<'a>(api: &'a HfApi, token: Option<String>, stop: &CancellationToken) -> Context<'a> {
    Context { http: api.http(), token, verify: true, limiter: None, stop: stop.clone() }
}

// ----------------------------------------------------------------- API

#[tokio::test]
async fn search_parses_results_and_is_cached() {
    let (base, stub) = start_stub(vec![]).await;
    let api = api(&base, None);
    let r = api.search("model", SearchSort::Downloads, 30).await.expect("search");
    assert_eq!(r.len(), 2);
    assert_eq!(
        (r[0].id.as_str(), r[0].author.as_str(), r[0].downloads, r[0].gated),
        ("org/Model-GGUF", "org", 1234, false)
    );
    assert!(r[1].gated, "gated: \"manual\" counts as gated");
    api.search("MODEL", SearchSort::Downloads, 30).await.expect("cached");
    assert_eq!(stub.searches.load(Ordering::SeqCst), 1, "second search served from the cache");
    api.search("model", SearchSort::Likes, 30).await.expect("other sort");
    assert_eq!(stub.searches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn listing_follows_pagination_and_reads_lfs_hashes_and_the_commit() {
    let (a, b) = (content(1), content(2));
    let (base, _stub) = start_stub(vec![("a-Q4_K_M.gguf", a.clone()), ("b-Q8_0.gguf", b.clone())]).await;
    let listing = api(&base, None).listing("org/Model-GGUF", "main").await.expect("listing");
    assert_eq!(listing.sha.as_deref(), Some("deadbeefcafe"));
    let by_path: HashMap<_, _> = listing.files.iter().map(|f| (f.path.as_str(), f)).collect();
    assert_eq!(by_path.len(), 3, "both pages were read; the directory entry was skipped");
    assert_eq!(by_path["a-Q4_K_M.gguf"].sha256.as_deref(), Some(sha(&a).as_str()));
    assert_eq!(by_path["b-Q8_0.gguf"].size, b.len() as u64, "the LFS size is used, not the pointer size");
    assert_eq!(by_path["README.md"].sha256, None, "non-LFS files have no sha-256");
}

#[tokio::test]
async fn error_statuses_map_to_actionable_errors() {
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", content(1))]).await;
    let plain = api(&base, None);
    assert_eq!(plain.listing("org/missing", "main").await.map(|_| ()), Err(HfError::NotFound));
    stub.behavior.lock().expect("lock").gated = true;
    assert_eq!(plain.listing("org/Model-GGUF", "main").await.map(|_| ()), Err(HfError::NeedsAccess));
    assert!(api(&base, Some("secret")).listing("org/Model-GGUF", "main").await.is_ok(), "the token is sent");
    assert!(HfError::NeedsAccess.to_string().contains("access token"));
    assert!(HfError::RateLimited(12).to_string().contains("12 seconds"));
}

// ------------------------------------------------------------ download

#[tokio::test]
async fn downloads_a_file_and_verifies_its_checksum() {
    let data = content(1);
    let (base, _s) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m-Q4_K_M.gguf");
    let stop = CancellationToken::new();
    let mut last = 0;
    download_file(&ctx(&api, None, &stop), &job(&api, dest.clone(), &data), |n| last = n).await.expect("download");
    assert_eq!(std::fs::read(&dest).expect("read"), data);
    assert!(!part_path(&dest).exists(), "no .part left behind");
    assert_eq!(last, data.len() as u64);
}

#[tokio::test]
async fn a_dropped_connection_resumes_with_a_range_request_and_still_verifies() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").drop_after = Some(1024 * 1024);
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m-Q4_K_M.gguf");
    let stop = CancellationToken::new();

    let first = download_file(&ctx(&api, None, &stop), &job(&api, dest.clone(), &data), |_| {}).await;
    assert!(matches!(first, Err(DownloadError::Network(_))), "the cut connection is reported: {first:?}");
    let partial = std::fs::metadata(part_path(&dest)).expect(".part kept").len();
    assert!(partial > 0 && partial < data.len() as u64, "partial file of {partial} bytes");

    download_file(&ctx(&api, None, &stop), &job(&api, dest.clone(), &data), |_| {}).await.expect("resume");
    assert_eq!(stub.range_requests.load(Ordering::SeqCst), 1, "resumed with Range, not from scratch");
    assert_eq!(std::fs::read(&dest).expect("read"), data, "the hash covers the old and the new bytes");
}

#[tokio::test]
async fn a_server_that_ignores_range_restarts_from_zero() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m-Q4_K_M.gguf");
    std::fs::write(part_path(&dest), &data[..500_000]).expect("seed part");
    stub.behavior.lock().expect("lock").ignore_range = true;
    download_file(&ctx(&api, None, &CancellationToken::new()), &job(&api, dest.clone(), &data), |_| {})
        .await
        .expect("download");
    assert_eq!(stub.range_requests.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(&dest).expect("read"), data);
}

#[tokio::test]
async fn corrupt_data_is_rejected_and_deleted() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").corrupt = true;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m-Q4_K_M.gguf");
    let r = download_file(&ctx(&api, None, &CancellationToken::new()), &job(&api, dest.clone(), &data), |_| {}).await;
    assert_eq!(r, Err(DownloadError::HashMismatch));
    assert!(!dest.exists() && !part_path(&dest).exists(), "nothing damaged is kept");
}

#[tokio::test]
async fn verification_can_be_switched_off() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").corrupt = true;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m.gguf");
    let mut c = ctx(&api, None, &CancellationToken::new());
    c.verify = false;
    download_file(&c, &job(&api, dest.clone(), &data), |_| {}).await.expect("download without verification");
    assert!(dest.exists());
}

#[tokio::test]
async fn gated_repositories_need_the_token() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").gated = true;
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m.gguf");
    let a = api(&base, None);
    let denied = download_file(&ctx(&a, None, &CancellationToken::new()), &job(&a, dest.clone(), &data), |_| {}).await;
    assert_eq!(denied, Err(DownloadError::NeedsAccess));
    assert!(denied.expect_err("denied").to_string().contains("licence"));
    download_file(&ctx(&a, Some("secret".into()), &CancellationToken::new()), &job(&a, dest.clone(), &data), |_| {})
        .await
        .expect("with token");
}

#[tokio::test]
async fn an_existing_valid_file_is_not_downloaded_again() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m.gguf");
    std::fs::write(&dest, &data).expect("seed");
    download_file(&ctx(&api, None, &CancellationToken::new()), &job(&api, dest, &data), |_| {}).await.expect("no-op");
    assert_eq!(stub.full_requests.load(Ordering::SeqCst) + stub.range_requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pausing_keeps_the_partial_file_and_resuming_completes_it() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("m-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").slow = true;
    let api = api(&base, None);
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join("m.gguf");
    let stop = CancellationToken::new();
    let stopper = stop.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(250)).await;
        stopper.cancel();
    });
    let r = download_file(&ctx(&api, None, &stop), &job(&api, dest.clone(), &data), |_| {}).await;
    assert_eq!(r, Err(DownloadError::Stopped));
    let kept = std::fs::metadata(part_path(&dest)).expect(".part kept").len();
    assert!(kept > 0 && kept < data.len() as u64);

    stub.behavior.lock().expect("lock").slow = false;
    download_file(&ctx(&api, None, &CancellationToken::new()), &job(&api, dest.clone(), &data), |_| {})
        .await
        .expect("resume");
    assert_eq!(std::fs::read(&dest).expect("read"), data);
}

// --------------------------------------------------------------- queue

fn queue_cfg(dir: &std::path::Path, max: u32) -> watch::Receiver<Arc<Config>> {
    let mut c = Config::default();
    c.downloads.models_dir = Some(dir.to_string_lossy().into_owned());
    c.downloads.max_concurrent = max;
    watch::channel(Arc::new(c)).1
}

fn spec(label: &str, file: &str, data: &[u8]) -> DownloadSpec {
    DownloadSpec {
        repo: "org/Model-GGUF".into(),
        revision: "main".into(),
        commit_sha: Some("deadbeefcafe".into()),
        files: vec![RepoFile { path: file.into(), size: data.len() as u64, sha256: Some(sha(data)) }],
        label: label.into(),
    }
}

async fn wait_state(m: &DownloadManager, id: u64, want: DownloadState) {
    for _ in 0..200 {
        if m.list().await.iter().any(|d| d.id == id && d.state == want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "download {id} never reached {want:?}: {:?}",
        m.list().await.iter().map(|d| (d.id, d.state)).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn the_queue_runs_downloads_one_at_a_time_and_reports_progress() {
    let (a, b) = (content(1), content(2));
    let (base, stub) = start_stub(vec![("a-Q4_K_M.gguf", a.clone()), ("b-Q8_0.gguf", b.clone())]).await;
    stub.behavior.lock().expect("lock").slow = true;
    let dir = tempfile::tempdir().expect("dir");
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let m = DownloadManager::spawn(api(&base, None), queue_cfg(dir.path(), 1), events);

    let id1 = m.enqueue(spec("A", "a-Q4_K_M.gguf", &a)).await.expect("enqueue");
    let id2 = m.enqueue(spec("B", "b-Q8_0.gguf", &b)).await.expect("enqueue");
    wait_state(&m, id1, DownloadState::Downloading).await;
    let list = m.list().await;
    assert_eq!(list.iter().find(|d| d.id == id2).map(|d| d.state), Some(DownloadState::Queued), "max_concurrent = 1");

    wait_state(&m, id2, DownloadState::Done).await;
    let done = m.list().await;
    let first = done.iter().find(|d| d.id == id1).expect("first");
    assert_eq!(first.state, DownloadState::Done);
    assert_eq!(first.bytes_done, first.bytes_total);
    assert!(first.paths[0].ends_with("a-Q4_K_M.gguf"));
    assert!(dir.path().join("org").join("Model-GGUF").join("a-Q4_K_M.gguf").exists());

    // The UI was told as it went.
    let mut saw_progress = false;
    let mut saw_finish = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            AppEvent::Download(d) if d.state == DownloadState::Downloading && d.bytes_done > 0 => saw_progress = true,
            AppEvent::Notification { title, .. } if title == "Download finished" => saw_finish = true,
            _ => {}
        }
    }
    assert!(saw_progress && saw_finish);
}

#[tokio::test]
async fn pause_resume_and_cancel_work_through_the_queue() {
    let a = content(1);
    let (base, stub) = start_stub(vec![("a-Q4_K_M.gguf", a.clone())]).await;
    stub.behavior.lock().expect("lock").slow = true;
    let dir = tempfile::tempdir().expect("dir");
    let m = DownloadManager::spawn(api(&base, None), queue_cfg(dir.path(), 1), EventBus::new());
    let id = m.enqueue(spec("A", "a-Q4_K_M.gguf", &a)).await.expect("enqueue");
    wait_state(&m, id, DownloadState::Downloading).await;
    // Let some bytes arrive first, so there is a partial file to keep.
    for _ in 0..100 {
        if m.list().await.first().is_some_and(|d| d.bytes_done > 0) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    m.pause(id);
    wait_state(&m, id, DownloadState::Paused).await;
    let part = dir.path().join("org").join("Model-GGUF").join("a-Q4_K_M.gguf.part");
    assert!(part.exists(), "pausing keeps the partial file");

    stub.behavior.lock().expect("lock").slow = false;
    m.resume(id);
    wait_state(&m, id, DownloadState::Done).await;
    assert!(stub.range_requests.load(Ordering::SeqCst) >= 1, "resumed with Range");

    // A second download is cancelled mid-way: partial data is deleted.
    stub.behavior.lock().expect("lock").slow = true;
    let b = content(2);
    let (base2, stub2) = start_stub(vec![("b-Q8_0.gguf", b.clone())]).await;
    stub2.behavior.lock().expect("lock").slow = true;
    let dir2 = tempfile::tempdir().expect("dir");
    let m2 = DownloadManager::spawn(api(&base2, None), queue_cfg(dir2.path(), 1), EventBus::new());
    let id2 = m2.enqueue(spec("B", "b-Q8_0.gguf", &b)).await.expect("enqueue");
    wait_state(&m2, id2, DownloadState::Downloading).await;
    m2.cancel(id2);
    wait_state(&m2, id2, DownloadState::Cancelled).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !dir2.path().join("org").join("Model-GGUF").join("b-Q8_0.gguf.part").exists(),
        "cancel deletes the partial file"
    );
}

#[tokio::test]
async fn failures_are_reported_and_can_be_retried() {
    let data = content(1);
    let (base, stub) = start_stub(vec![("a-Q4_K_M.gguf", data.clone())]).await;
    stub.behavior.lock().expect("lock").corrupt = true;
    let dir = tempfile::tempdir().expect("dir");
    let m = DownloadManager::spawn(api(&base, None), queue_cfg(dir.path(), 1), EventBus::new());
    let id = m.enqueue(spec("A", "a-Q4_K_M.gguf", &data)).await.expect("enqueue");
    wait_state(&m, id, DownloadState::Failed).await;
    let failed = m.list().await.remove(0);
    assert!(failed.error.as_deref().unwrap_or("").contains("damaged"), "{:?}", failed.error);

    stub.behavior.lock().expect("lock").corrupt = false;
    m.resume(id);
    wait_state(&m, id, DownloadState::Done).await;
}

#[tokio::test]
async fn unsafe_remote_file_names_are_refused_at_enqueue() {
    let (base, _s) = start_stub(vec![]).await;
    let dir = tempfile::tempdir().expect("dir");
    let m = DownloadManager::spawn(api(&base, None), queue_cfg(dir.path(), 1), EventBus::new());
    let evil = DownloadSpec {
        repo: "org/Model-GGUF".into(),
        revision: "main".into(),
        commit_sha: None,
        files: vec![RepoFile { path: "../../evil.gguf".into(), size: 1, sha256: None }],
        label: "evil".into(),
    };
    let err = m.enqueue(evil).await.expect_err("refused");
    assert!(err.contains("not a safe file name"), "{err}");
    assert!(m.enqueue(DownloadSpec { files: vec![], ..spec("x", "x", b"x") }).await.is_err());
}

#[tokio::test]
async fn downloads_pause_while_a_game_runs_when_configured() {
    let a = content(1);
    let (base, stub) = start_stub(vec![("a-Q4_K_M.gguf", a.clone())]).await;
    stub.behavior.lock().expect("lock").slow = true;
    let dir = tempfile::tempdir().expect("dir");
    let mut c = Config::default();
    c.downloads.models_dir = Some(dir.path().to_string_lossy().into_owned());
    c.downloads.pause_while_gaming = true;
    let (_tx, rx) = watch::channel(Arc::new(c));
    let m = DownloadManager::spawn(api(&base, None), rx, EventBus::new());
    let id = m.enqueue(spec("A", "a-Q4_K_M.gguf", &a)).await.expect("enqueue");
    wait_state(&m, id, DownloadState::Downloading).await;

    m.set_gaming(true);
    wait_state(&m, id, DownloadState::PausedForGame).await;
    stub.behavior.lock().expect("lock").slow = false;
    m.set_gaming(false);
    wait_state(&m, id, DownloadState::Done).await;
}
