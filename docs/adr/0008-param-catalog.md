# ADR-0008: Parameter catalog generated from llama-server --help

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

llama-server has 100+ flags that change between releases.

## Decision

Parse --help into a catalog; merge a curated overlay; unparseable entries become free-text fields.

## Consequences

Every flag of the installed engine is editable; nothing is hidden.
