# ADR-0002: One process, WebView created on demand

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

Two processes (service + GUI) were considered. A Tauri v2 app can run tray-only and create/destroy its WebView at will (F12).

## Decision

Run core and tray in one process. Create the settings window when opened and destroy it when closed. Keep all logic in `gatekeeper-core`, which has no Tauri dependency.

## Consequences

No IPC layer. Idle cost equals a tray-only app. A headless CLI binary still runs the core for tests.
