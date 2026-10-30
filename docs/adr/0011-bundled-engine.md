# ADR-0011: Ship llama.cpp and runtime libraries inside the installer

**Status:** Accepted (owner request, supersedes the "bring your own engine until Phase 5" sequencing in spec §16).

## Context

The owner wants the app to come with every dependency it needs, including llama.cpp, so nothing must be installed separately.

## Decision

- A build step (`tools/fetch-engine.ps1`) downloads a pinned llama.cpp Windows release (backend assets per `engine-rules.json`), the matching CUDA runtime (`cudart`) zip, and the WebView2 bootstrapper, verifies their SHA-256, and stages them under `app/src-tauri/resources/`.
- The Tauri bundler ships `resources/` in the per-user installer. On first run the app copies/uses the bundled engine as the default `engine.llama_server_dir`.
- Users can still point at their own build, or (Phase 5) download newer engines; the bundled one is the rollback target.
- The Rust runtime is linked statically (`+crt-static`) so no Visual C++ redistributable is needed.

## Consequences

Larger installer (hundreds of MB with CUDA). Releases must re-pin the engine version deliberately. No separate user setup.
