# Manual test matrix (spec §22.2)

Automated tests cannot cover real games, drivers or hardware. Run these before a release and record the result.

| # | Scenario | Expected | Result |
|---|---|---|---|
| 1 | Steam game launch with a model loaded | Requests get `game_running`; VRAM freed; reload after exit + cooldown | Partial (2026-10-21): a renamed ping.exe stood in for a game; the real model was gated, refused with `game_running`, and reloaded after the cooldown. A real Steam game has not been run. |
| 2 | Non-Steam game in the games list | Same as 1 | |
| 3 | Exclusive full-screen game not in the list | Gate closes via full-screen detector | |
| 4 | Borderless game with heuristic enabled | Gate closes after sustain period | |
| 5 | NVIDIA sysmem fallback ON, oversized model | Spill detected; overflow action applied | |
| 6 | NVIDIA sysmem fallback OFF, oversized model | `model_load_failed` | |
| 7 | AMD / Intel (Vulkan) model load + pressure | Pressure unload works | |
| 8 | Sleep / resume with a loaded model | Children health-checked; games rescanned | |
| 9 | LAN mode with API key | Unauthenticated requests get 401; bad Host rejected | Partial: authentication, Host and CORS checks are covered by automated tests (`app_tests`, `proxy_tests`); no second machine has been used yet. |
| 10 | Long stream at game launch, each drain policy | finish / wait_then_cancel / cancel behave per §7.4 | Partial: all three policies pass against the fake server; the real model passed with `wait_then_cancel` (`real_engine_e2e`, 14 of 14 runs). |
| 11 | Two models loaded, pressure from another app | LRU unload until below threshold | |
| 12 | Gated Hugging Face download | Clear 401/403 message; works with token | Not run. The 401/403 handling and token path are covered by stub tests; the live Hub test used a public repo. |
| 13 | Interrupted then resumed download | Resumes with Range; hash verifies | Pass (2026-10-22): paused at 27% in the window, resumed with a Range request, SHA-256 verified, model added and answered a chat request through the proxy. |
| 14 | 24 h idle soak | NFR-1 met, no memory growth | Partial: a 10-minute soak with a loaded model was flat (see phase0-findings.md). The 24 h run has not been done. |
