# Public source validation - October 29, 2026

These checks apply to the curated source copy prepared for the initial public alpha. They do not establish hardware compatibility or production readiness.

| Check | Result |
|---|---|
| Rust formatting: `cargo fmt --all --check` | Passed |
| Core library tests, with the GPU/nvidia-smi hardware comparison explicitly skipped | 347 passed, 0 failed, 1 filtered out |
| Captured llama.cpp output fixture tests | 8 passed, 0 failed |
| Svelte type/diagnostic check | 0 errors, 0 warnings |
| UI ESLint | Passed |
| Vite production build | Passed, 169 modules transformed |

Rust checks ran offline using cached dependencies and a local temporary build directory. UI checks ran on a fresh temporary copy of `app/ui`, with cached/offline dependencies. No real engine, model, game, desktop application, installer, or hardware-comparison test was launched for these checks. The initial restricted test run could not access local test sockets, a temporary registry key, or a temporary test credential; the complete core suite passed when rerun with that test access available.

## Prepublication change

AI-assisted hardening now rejects disallowed, malformed, or duplicated browser Origin headers before request dispatch, including administrative mutations. Native clients without an Origin header and explicitly configured trusted origins remain supported. New regression tests check rejection without state mutation and normal trusted/native behavior. These regressions are included in the passing core suite above.

## Scope of the publication copy

- Includes source, lockfiles, icons, build/development tools, design documentation, and tests.
- Excludes installed settings, credentials, models, engines/runtime binaries, installers/build outputs, `node_modules`, private AI planning notes, and generated Tauri schemas/permission files.
- Retains historical captured output only as parser fixtures; absolute machine paths in those fixture logs were redacted and the fixture README records that change.
- A limited common-token/private-key and personal-path scan found no matches in the reviewed source copy. This is not a comprehensive secret or security audit.
- AI-generated implementation and Kevin's reported daily use/manual testing are disclosed in the README.

Desktop builds/packaging, clean-account installation/uninstallation, real games, real GPU-memory pressure, AMD/Intel hardware, sleep/resume, second-machine LAN use, and the 24-hour soak remain separate acceptance work. See the manual test matrix; historical development notes are explicitly labeled as historical.
