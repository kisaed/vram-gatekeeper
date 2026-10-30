# ADR-0012: Hand-written UI types with a drift test instead of tauri-specta

**Status:** Accepted (revises spec decision D10 and §24).

## Context

Spec D10 asked for TypeScript bindings generated from Rust with `tauri-specta`. When the UI was built (October 2026):

- `tauri-specta` and `specta` were still release candidates (`2.0.0-rc.26`, `2.0.0-rc.27`), and `specta-typescript` was `0.0.x`.
- Using it would require deriving `specta::Type` on every core type the UI sees (`Config` and all its sections, status, events), coupling the platform-neutral core crate to pre-release crates.
- `crates.io` already lists Tauri 3.0 alphas as the newest `tauri`; the project targets the stable Tauri 2 line.

## Decision

- Keep the core free of UI-framework and pre-release dependencies.
- Write `app/ui/src/lib/types.ts` by hand, mirroring the serde shapes.
- Add `crates/gatekeeper-core/tests/ui_types_drift.rs`: it serialises real Rust values (a populated `Config`, `StatusSnapshot`, `AdapterInfo`, every `AppEvent` variant) and fails if any JSON key is not declared in `types.ts`, or an event type is missing from the `CoreEvent` union.
- Only commands listed in `app/src-tauri/build.rs` can be invoked from the page; `capabilities/default.json` mirrors that list.

## Consequences

Adding or renaming a Rust field without updating the TypeScript fails `cargo test`. The guard checks key names (as whole property names, so `id:` is not satisfied by `model_id:`) and the values of the string enums the UI switches on (model states, gate indicator, block reasons, unload causes, parameter kinds). It does not check field types; those are caught by `svelte-check` against the hand-written types and by the UI working against the real app. Revisit when `tauri-specta` reaches 2.0 stable.
