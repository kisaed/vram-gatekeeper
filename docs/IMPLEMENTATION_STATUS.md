# Implementation status

> Historical AI-assisted development notes. Test totals and hardware measurements below were recorded during development; they are not a fresh independent verification of this public source copy. The manual acceptance matrix still contains unfinished scenarios.

Legend: ✅ done and tested · 🟡 partial · ⬜ not started · 🔧 needs real hardware/games (manual)

## Environment
- Rust 1.99.1 + MSVC installed by the owner (build-time only; ADR-0011).
- The repo lives on a network drive (Z:), which Windows refuses to execute build output from. **Set `CARGO_TARGET_DIR` to a local folder**, e.g. `C:/Users/<you>/cargo-target/vram-gatekeeper`.
- Bundled dependencies (llama.cpp, cudart, WebView2 bootstrapper): NFR-7. `tools/fetch-engine.ps1` downloads the pinned, checksummed engines; the per-user NSIS installer packs both engines and embeds the WebView2 bootstrapper (done). Installers are built with `tools/build.ps1` into `builds/<version>-build.<NNNN>/` (spec §25.4.1). The clean-machine acceptance run is still to do (see NFR-7 below).
- UI: `pwsh tools/ui.ps1 check|lint|build` (mirrors to a local folder when the repo is on a network drive).

## Test totals (last run)
`cargo test --workspace`: 345 core unit/state-machine/property/proxy/Hugging Face/updater/speculative tests, 10 real-process end-to-end tests with the fake server, 8 real-fixture tests, 10 UI type-drift tests, 7 host tests, all passing.
With `GATEKEEPER_REAL_ENGINE` / `GATEKEEPER_REAL_MODEL` set (and `--test-threads=1`, because the GPU-measuring tests compare adapter-wide memory), 7 more tests run the **real llama.cpp engine and model** on the RTX 5090, including speculative decoding. `GATEKEEPER_LIVE_HF=1` runs one live Hugging Face test.
`cargo clippy --workspace --all-targets -- -D warnings`: clean. UI: `svelte-check` 0 errors, `eslint` clean.
Phase 0 and Tauri measurements: see [phase0-findings.md](phase0-findings.md).

## Phases
| Phase | Status |
|---|---|
| 0 Spikes | 🟡 PDH, per-process attribution, release timing, layer log, fit, job object done on real hardware; real games, NVIDIA sysmem spill and Tauri idle footprint remain (see phase0-findings.md) |
| 1 Headless core | ✅ config, supervisor, proxy, controller, process-scan detector, CLI, fake server |
| 2 Detection / VRAM rules | ✅ Sleep/resume handling is implemented and unit-tested (a real sleep cycle is in the manual matrix). Done: PDH+DXGI monitor, process scan, foreground hook, exclusive full-screen, Steam registry watch, Credential Manager store, Windows game list import (opt-in), heavy-GPU heuristic (opt-in; unit-tested and real counters read on hardware, not yet seen firing on a real game) |
| 3 GUI + tray | ✅ Tray, on-demand WebView window, all pages (Status with first-run welcome, Models with parameter editor and test load, Download, Games, VRAM, Engine, General, Logs with diagnostics export) verified in the real window |
| 4 HF downloads | ✅ search, variants with fit, resumable verified downloads, queue, library, add-as-model. Verified live against the Hub and through the GUI (download, pause, resume, SHA-256, chat through the proxy) |
| 5 Polish / bundling / installer | 🟡 Done: per-user NSIS installer with both engines (about 534 MiB), engine updater with rollback, model update checks, diagnostics bundle, test load, first-run welcome, sleep/resume. Remaining: installing it on a clean account, 24 h soak, app updater (Phase 6) |

## Requirements
| ID | Status | Tests / notes |
|---|---|---|
| FR-GAME-1 | ✅ | process list, foreground hook, full-screen, Steam (real-game verification pending: manual matrix) |
| FR-GAME-2..5 | ✅ | `gate::controller_tests` (game_*, drain_*), `proxy_tests`, `e2e::gate_works_end_to_end_with_real_processes` |
| FR-GAME-6 | ✅ | cooldown + `preload_after_game` tests |
| FR-GAME-7 | ✅ | `keep_policy_model_ignores_games` |
| FR-GAME-8 | ✅ | list + matching, GUI browse / running apps / check |
| FR-VRAM-1 | ✅ | PDH+DXGI monitor, verified against nvidia-smi |
| FR-VRAM-2,3 | ✅ | pressure tests (controller) |
| FR-VRAM-4,5 | ✅ | layer overflow verified against **real llama.cpp logs** and the real engine; driver spill logic unit-tested with scripted samples (real sysmem spill not yet reproduced) |
| FR-VRAM-6 | ✅ | loop guard tests |
| FR-VRAM-7 | ✅ | make-room tests |
| FR-REQ-1,2 | ✅ | OpenAI + Anthropic routes, aliases, `/v1/models`, passthrough |
| FR-REQ-3 | ✅ | queued admission with load timeout |
| FR-REQ-4,5 | ✅ | error formats, streaming error event, client-disconnect release |
| FR-MOD-1 | ✅ | catalog parser/argv verified against the **real b11804 `--help`**; GUI editor shows the generated catalog (233 params) |
| FR-MOD-2,3 | ✅ | single JSON file, atomic save, hot reload, invalid-edit rejection |
| FR-MOD-4 | ✅ | `hf::*` (35 tests with a stub Hub) + live Hub run + GUI run |
| FR-MOD-5 | ✅ | `render_command_line`, Launch and Test tab |
| FR-MOD-6 | ✅ | `files.draft`, `speculative::types` (parsing, kinds, launch arguments), Speculative Decoding tab; real engine: `real_engine_loads_with_the_model_as_its_own_draft`, `real_engine_loads_with_ngram_mod` |
| FR-MOD-7 | ✅ | `supervisor::footprint` (hash, estimate), `hf::library` (draft counts as a use) |
| FR-MOD-8 | ✅ | `hf::gguf_files` (sidecars), `hf::draft_search`, `hf::library::model_from_download`, `speculative::attach_draft`, Download page |
| FR-MOD-9 | ✅ | `speculative::findings` (every row of the §10.6 table), Test Load confirmations (`LogStore`, `parse_speculative_implementation`); real engine: `real_engine_reports_a_model_without_prediction_layers_that_is_asked_for_mtp` |
| Validation severities (spec §14.4) | ✅ | `config::validate` (`Severity`, `errors_of`), `config::store`, `app_tests::save_config_ignores_warnings_but_refuses_a_missing_required_draft` |
| Failed-request logging (spec §20) | ✅ | `proxy::failure_log`, `proxy_tests` (`upstream_error_status_is_logged_and_body_reaches_the_client`, `rejections_are_logged_at_warn`, `huge_or_non_utf8_error_bodies_do_not_break_forwarding`) |
| Uninstaller data removal (spec §25.4) | 🟡 | NSIS hook `app/src-tauri/windows/installer-hooks.nsh`; the generated installer script includes the hook file and inserts its macro, and makensis compiled it; running the uninstaller is not tested |
| FR-APP-1..4 | ✅ | Tauri host: tray, single instance, autostart plugin, notifications, installer built |
| NFR-1 | ✅ | release build, window closed: 21.5 MB private, 0.009% CPU, 0 WebView2 processes; with a model loaded 21.7 MB / 0.014% |
| NFR-2 | 🟡 | scan trigger and foreground hook tested with real processes; real games pending |
| NFR-3 | ✅ so far | nothing requires admin |
| NFR-4 | ✅ | design: Toolhelp snapshot only, no `OpenProcess` on games |
| NFR-5 | ✅ | job-object kill-on-close test; atomic config save |
| NFR-6 | 🟡 | `deny(missing_docs)` enforced; ongoing |
| NFR-7 | 🟡 | engine resolution/validation, fetch script and NSIS packaging (both engines, notices, WebView2 bootstrapper) done; the clean-machine acceptance run is still to do |
| Engine updates (spec §16.2) | ✅ | `engine::updater` (13 tests incl. stub GitHub, checksum mismatch, path escape); verified live: found build 11827, downloaded 556 MiB, verified, switched, ran a model on it, rolled back |
| FR-MOD-4 (update checks) | ✅ | `hf::update` + app test; verified live against the Hub |
| FR-APP-4 (diagnostics) | ✅ | zip with redacted config; checked that the bundle contains no profile path or secret |
| Sleep/resume (spec §21) | 🟡 | `ResumeWatcher` registers for real; resume callback and the controller health-check/restart logic are tested; a real sleep cycle is manual |

## Planned in the spec, not built

Everything the spec marks **Planned**, in one list:

- Closer footprint estimate: GPU-layer share, KV cache, all shards (§9.5).
- Games: "last seen running" per entry (§11.7, §14.3), live detection preview (§11.7, §17.3), removable built-in heavy-GPU exclusions (§11.5).
- GPU device suggestion matching the monitored adapter (§12.1).
- Overlay min/max ranges and units (§10.2); GGUF metadata summary on the Files tab (§10.5).
- Import and export of settings (§17.3); re-verifying downloaded file hashes (§15.5); generated `docs/config-reference.md` (§14.3).
- Parameter catalog generated at build time and shipped; build failure when `llama-server.exe` cannot report `--version`; Vulkan-only "lite" installer (§25.2, §16.1).
- Guided first-run setup (§21).
