# ADR-0013: Engine updates are user-started, official-only and checksum-verified

**Status:** Accepted (implements spec §16.2).

## Context

The installer bundles a pinned llama.cpp (ADR-0011), but new builds add model support and speed, so the app can download newer ones. A downloaded engine is an executable that the app then runs, which makes the download path a supply-chain risk. Phase 0 also found that `releases/latest` has no binaries; real builds are `bNNNNN` tags.

## Decision

- **User-started only.** Nothing checks or downloads in the background. "Check for a newer engine" and "Download" are buttons (no telemetry, spec §19).
- **Release list, not `latest`.** The app reads `releases?per_page=30` and picks the newest release that has a build for the chosen backend. A CUDA build is offered only with its matching `cudart` zip, and the CUDA version of the engine in use is preferred (a newer CUDA major may need a newer driver).
- **Backend from the GPU.** NVIDIA discrete GPU: CUDA. Other GPUs: Vulkan. Software adapter: CPU. The other backends are offered as alternatives.
- **Official and checksummed.** The core refuses any plan whose URL is not under `https://github.com/ggml-org/llama.cpp/releases/download/`, whose names are not plain file names, or whose asset has no SHA-256 published by GitHub. The window never supplies a plan: it names only a release tag and a backend, and the core builds the plan from the release list of the last update check, so the window cannot widen this. Progress reaches the window as events, and the files are fetched by the same downloader as model files (disk-space check, size and checksum verification).
- **All or nothing.** Files are downloaded to a scratch folder, hashed while streaming, unpacked with path-escape checks, and moved to `engines\<tag>-<backend>` only when everything succeeded. A failure leaves no half-installed engine.
- **Never the install directory.** Engines go to `%LOCALAPPDATA%\VRAM Gatekeeper\engines`. The bundled engine stays untouched, so "Go back to the bundled engine" always works.
- **Validate before switching.** Switching runs `--version` on the target first, saves the setting only if it runs, rebuilds the parameter catalog, and lists model settings the new engine no longer understands. Nothing is deleted.
- **Only known folders from the window.** The window may select only folders the app itself downloaded (or clear the setting). A custom folder goes through the settings form, which validates it.

## Consequences

A compromised window process cannot make the core download or run arbitrary code through this path. A release without a published digest cannot be installed, which is the safe failure. GitHub's unauthenticated rate limit (60 requests per hour) is fine for user-started checks and is reported in words if hit.
