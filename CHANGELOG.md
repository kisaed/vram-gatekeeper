# Changelog

## Unreleased
- First installer release (v0.1.0 pre-release), published as a GitHub release asset; README gains install instructions.
- Public source alpha: AI-assistance disclosure, architecture diagrams, source-build instructions, MIT license, source checks, and explicit remaining manual-validation work.
- AI-assisted prepublication hardening: reject unapproved, malformed, and duplicate browser Origin headers before dispatch; preserve native clients and configured trusted origins. Regression tests cover rejected requests without state mutation.
- Speculative decoding (separate draft model, EAGLE-3, DFlash, DSpark, Multi-Token Prediction and n-gram types): a Speculative Decoding tab on each model, a `files.draft` setting, findings before a model loads, memory accounting, a Test Load report of which types started, and Hugging Face support (draft sidecar files, Add as a Draft Model, Find Draft Models).
- Settings findings now have a severity (error, warning or note); only errors block saving or reject a hand edit. A reserved key in a model's parameters is a warning, so older files keep loading.
- Failed requests (refusals by the gatekeeper and error answers from llama-server) are logged at warning level with the model, endpoint, status and the start of the error message.
- The uninstaller's "Delete the application data" box also removes the settings folder, state, logs, downloaded engines and stored secrets; downloaded models are never removed.
- Status page shows the connection details (OpenAI- and Anthropic-compatible base URLs, API key requirement) and the model name to enter in the client for each model.
- All interface text reworded to a formal, professional style; explanations now appear on hover instead of under each setting.
- Model parameters are shown with readable names (for example "Maximum Logical Batch Size") instead of command-line flags; the flags remain in the hover text and in search.
- `tools/build.ps1` files each installer build under `builds/<version>-build.<NNNN>/`; old builds are kept.
- Core: config store with hot reload, per-model llama-server supervisor, OpenAI/Anthropic-compatible proxy,
  gate controller (games, VRAM pressure, overflow, loop guard, make-room), Windows detectors
  (process list, foreground, exclusive full-screen, Steam, Windows game list, optional heavy-GPU heuristic).
- Parameter editor generated from `llama-server --help`.
- Tauri tray app with an on-demand settings window (Status, Models, Download, Games, VRAM and requests,
  Engine, General, Logs).
- Hugging Face search, resumable checksum-verified downloads, library and add-as-model.
- Test load and diagnostics bundle (secrets and personal paths removed).
- Engine updates from GitHub (user-started, official URLs only, SHA-256 verified), with rollback to the bundled engine.
- Model update checks against Hugging Face.
- Sleep/resume: models are health-checked after the PC wakes and restarted if they no longer answer.
- Switching engines marks loaded models as needing a restart.
- Per-user NSIS installer bundling the Vulkan and CUDA engines.
- Bundled engine: `tools/fetch-engine.ps1` stages the pinned, checksummed llama.cpp builds for the installer.
- Project skeleton, ADRs 0001-0012, workspace lints.
