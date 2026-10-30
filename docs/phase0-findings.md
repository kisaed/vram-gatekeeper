# Phase 0 findings (measured on the development machine)

> Historical AI-assisted development notes. Test totals and hardware measurements below were recorded during development; they are not a fresh independent verification of this public source copy. The manual acceptance matrix still contains unfinished scenarios.

Machine: Windows 11, NVIDIA GeForce RTX 5090 (32 GiB), driver as installed 2026-10-21.
Engine: llama.cpp **b11804**, CUDA 13.4 build. Model: Qwen2.5-0.5B-Instruct Q4_K_M.
Fixtures from these runs are in `crates/gatekeeper-core/tests/fixtures/`.

| Spike (spec §26) | Result |
|---|---|
| PDH counters | **Confirmed.** `\GPU Adapter Memory(*)\Dedicated Usage` read 3626 MiB while `nvidia-smi` read 3603 MiB (23 MiB apart). DXGI total 32187 MiB vs 32607 MiB from nvidia-smi (DXGI excludes a reserved slice). 61 processes appear in the per-process counters. Test: `gpu::windows_monitor::tests::adapter_counters_agree_with_nvidia_smi`. |
| Per-process attribution | **Confirmed.** Loading the model (ctx 65536) grew adapter use by 1738 MiB; the llama-server process counter said 1737 MiB. Test: `real_engine_e2e::real_gpu_counters_attribute_the_model_and_show_vram_released_after_unload`. |
| Kill and release timing | **Fast.** Unload to `unloaded` took 1.10 s (kill, process reaped, adapter usage back at exactly the 3626 MiB baseline). The 3 s `stop_release_wait_ms` ceiling is conservative. |
| Layer-offload log line | **Confirmed, with a catch.** `load_tensors: offloaded 25/25 layers to GPU` exists, but **only at `--verbosity 4`**. The default (3) prints no placement at all, so the overflow check would silently never fire. The launcher now passes `--verbosity 4` unless the user set one. |
| `--fit` behaviour | `--fit` is an `on`/`off` enum, default **on**. When the free-memory target cannot be met it sheds layers one at a time and the final line reads `offloaded 0/25 layers to GPU`. Verified end to end: `real_fit_moving_layers_to_the_cpu_is_caught_as_overflow`. With an explicit `-ngl 10` the line reads `offloaded 10/25` and is correctly **not** treated as overflow. |
| `/health` during load | **Confirmed.** No answer while the process initialises (about 4 s on the first CUDA run), then 503, then 200. |
| Stream cancellation (F9) | **Confirmed for the proxy path.** Dropping the upstream body ends generation (verified against a stub, the fake server, and the real server through the gate: the stream ends with the in-stream error and the process is killed). The slot-release log line of the *real* server on a client disconnect has not been captured separately. |
| Job object | **Confirmed.** Killing the parent process kills the llama-server child (`e2e::children_die_when_the_parent_is_killed`). |
| `--version` / `--help` streams | `--version` writes to **stderr** (stdout empty); `--help` writes 750 lines to stdout. |
| Release assets | `releases/latest` points at a tag with **no binaries** (`v0.7.0`, only `nightly-tag.txt`). Binary releases are the `bNNNNN` tags. The engine updater must list releases and pick the newest with matching assets. GitHub publishes SHA-256 digests in the release API, which `fetch-engine.ps1 -UpdatePins` uses. |
| Removed flags | This build has **no `--mmap` / `--mlock`** (replaced by a load-mode option). The generated catalog reflects that; hard-coded flag lists would have been wrong. |
| Detection with real games | **Not yet done** (needs Steam games / exclusive full-screen titles). A renamed `ping.exe` stands in for a game in the automated tests; the real foreground hook, Steam and full-screen detectors still need the manual matrix. |
| Spill (sysmem fallback) | **Not yet done.** Needs a model larger than free VRAM with NVIDIA's sysmem fallback enabled. The detection logic is unit tested with scripted samples. |
| Tauri idle footprint / WebView2 teardown | **Confirmed (release build, defaults).** Idle with the window closed: 21.5 MB private memory, 0.009% total CPU, **0** WebView2 processes owned by the app. With a model loaded: 21.7 MB, 0.014%. Opening the window starts 6 WebView2 processes; closing it returns to 0 while the tray app and proxy keep running. Killing the app leaves no llama-server behind. |
| Windows game list (GameConfigStore Children) | **Readable, but thin.** 66 child keys on the development machine, only 6 with `MatchedExeFullPath` (and one is `javaw.exe`). Kept as an opt-in import, not an automatic source. |
| Heavy-GPU heuristic cost | The GPU Engine utilisation counter returns about 1,600 instances. With the heuristic on, a debug build uses about 0.8% of one core; the counter is only read while the foreground program is not excluded. Off by default. |
| 10-minute soak (release build) | 2026-10-22, model loaded, one chat request every 10 s: 59 of 59 succeeded; private memory 21.9 MB to 22.3 MB, handles 331 to 330, threads 17 to 16; app CPU 0.48% of one core (0.015% of all cores) with GPU sampling at the fast rate. This is a smoke test; the 24 h soak (matrix row 14) is still to run. |
