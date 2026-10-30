//! Phase 4 acceptance against the real Hugging Face Hub (spec §26 Phase 4).
//!
//! Needs the network, so it only runs when `GATEKEEPER_LIVE_HF=1`:
//! search, list, group, download the smallest quantisation of a small model with
//! a deliberate interruption, resume, and verify the SHA-256 published by the Hub.

use std::sync::Arc;
use std::time::Duration;

use gatekeeper_core::hf::api::{HUB_URL, HfApi, SearchSort};
use gatekeeper_core::hf::download::{Context, FileJob, download_file, hash_file, part_path};
use gatekeeper_core::hf::gguf_files::{Fit, fit_for, group_files};
use tokio_util::sync::CancellationToken;

fn enabled() -> bool {
    std::env::var("GATEKEEPER_LIVE_HF").is_ok_and(|v| v == "1")
}

#[tokio::test]
async fn search_list_download_interrupt_resume_and_verify_against_the_real_hub() {
    if !enabled() {
        eprintln!("skipped: set GATEKEEPER_LIVE_HF=1 to run against huggingface.co");
        return;
    }
    let api = HfApi::new(HUB_URL, Arc::new(|| None)).expect("client");

    // 1. Search.
    let results = api.search("qwen2.5 0.5b instruct", SearchSort::Downloads, 20).await.expect("search");
    eprintln!(
        "search returned {} repositories; first: {:?}",
        results.len(),
        results.first().map(|r| (&r.id, r.downloads))
    );
    let repo = "Qwen/Qwen2.5-0.5B-Instruct-GGUF";
    assert!(results.iter().any(|r| r.id == repo), "the official GGUF repo is found by search");

    // 2. List and group.
    let listing = api.listing(repo, "main").await.expect("listing");
    eprintln!("commit {:?}, {} files", listing.sha, listing.files.len());
    assert!(listing.sha.is_some(), "the Hub reports the commit sha");
    let grouped = group_files(&listing.files);
    for v in &grouped.variants {
        eprintln!(
            "  variant {:<40} {:<8} {:>7.1} MiB  fit on 32 GiB: {:?}",
            v.id,
            v.quant,
            v.total_bytes as f64 / 1048576.0,
            fit_for(v.total_bytes, 32 * 1024)
        );
    }
    assert!(grouped.variants.len() >= 3, "several quantisations are offered");
    let smallest = grouped.variants.first().expect("a variant");
    assert!(smallest.files.iter().all(|f| f.sha256.is_some()), "LFS files carry a published SHA-256");
    assert_eq!(fit_for(smallest.total_bytes, 32 * 1024), Fit::FitsFully);

    // 3. Download with an interruption at roughly 25%.
    let file = &smallest.files[0];
    let dir = tempfile::tempdir().expect("dir");
    let dest = dir.path().join(&file.path);
    let job = FileJob {
        url: api.resolve_url(repo, "main", &file.path),
        dest: dest.clone(),
        size: file.size,
        sha256: file.sha256.clone(),
        trust_existing: false,
    };

    let stop = CancellationToken::new();
    let quarter = file.size / 4;
    let stopper = stop.clone();
    let first = {
        let ctx = Context { http: api.http(), token: None, verify: true, limiter: None, stop: stop.clone() };
        download_file(&ctx, &job, move |done| {
            if done >= quarter {
                stopper.cancel();
            }
        })
        .await
    };
    eprintln!("first attempt: {first:?}");
    assert!(first.is_err(), "interrupted");
    let kept = std::fs::metadata(part_path(&dest)).expect(".part kept").len();
    eprintln!("kept {} of {} bytes ({:.0}%)", kept, file.size, kept as f64 * 100.0 / file.size as f64);
    assert!(kept > 0 && kept < file.size);

    // 4. Resume and verify.
    let started = std::time::Instant::now();
    let ctx = Context { http: api.http(), token: None, verify: true, limiter: None, stop: CancellationToken::new() };
    download_file(&ctx, &job, |_| {}).await.expect("resume completes and verifies");
    eprintln!("resumed and finished in {:?}", started.elapsed());
    assert_eq!(std::fs::metadata(&dest).expect("final file").len(), file.size);
    assert!(!part_path(&dest).exists());

    // 5. Independent re-hash against the Hub's published value.
    let actual =
        tokio::time::timeout(Duration::from_secs(120), hash_file(&dest)).await.expect("hash in time").expect("hash");
    assert_eq!(Some(actual), file.sha256.clone(), "matches the SHA-256 the Hub publishes");

    // 6. The GGUF header of the real file is readable.
    let meta = gatekeeper_core::gguf::header::read_meta(&dest).expect("real GGUF header");
    eprintln!("GGUF metadata: {meta:?}");
    assert_eq!(meta.architecture.as_deref(), Some("qwen2"));
    assert!(meta.block_count.is_some() && meta.context_length.is_some());
}
