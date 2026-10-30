# ADR-0006: Event-driven detection, light polling only where needed

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

Idle CPU must stay at or below 0.1%.

## Decision

Use SetWinEventHook, registry change notification, and a 3 s Toolhelp snapshot scan. No WMI, no process handles on games.

## Consequences

Anti-cheat friendly (NFR-4); near-instant detection via the foreground hook.
