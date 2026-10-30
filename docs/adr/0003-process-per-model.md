# ADR-0003: One llama-server per model, killed to unload

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

Router-mode sleep leaves ~600 MiB allocated per process (F10).

## Decision

Spawn one llama-server per model; unload with TerminateProcess; never use router mode.

## Consequences

Exact per-model VRAM attribution, guaranteed release, crash isolation.
