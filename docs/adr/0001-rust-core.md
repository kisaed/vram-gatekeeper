# ADR-0001: Rust for all always-on logic

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

The proxy, monitors and process supervisor run continuously next to games, so they must have a tiny idle footprint (NFR-1).

## Decision

Write the core in Rust (tokio, axum, hyper) with the `windows` crate for Win32.

## Consequences

No GC pauses and a small footprint. Unsafe Win32 code is quarantined in `platform/windows/`.
