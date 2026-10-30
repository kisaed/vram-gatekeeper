# ADR-0005: Vendor-neutral GPU monitoring via PDH + DXGI

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

NVIDIA, AMD and Intel must all be supported (spec §2.3).

## Decision

Use Windows GPU performance counters (English names via PdhAddEnglishCounterW) and DXGI for adapter info.

## Consequences

One code path for all vendors. Adapter-level counters drive decisions; per-process counters are for attribution only (F4).
