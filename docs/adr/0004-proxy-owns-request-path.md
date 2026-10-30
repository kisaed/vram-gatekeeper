# ADR-0004: The app owns the request path through a proxy

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

Gating only works if clients cannot bypass it.

## Decision

Clients talk only to the gatekeeper. Children bind to 127.0.0.1 on random ports with a random per-launch API key.

## Consequences

Every request passes the gate controller. Local web pages cannot reach llama-server directly.
