# ADR-0009: Own Hugging Face downloader

**Status:** Accepted (confirmed against spec.md §4; real-hardware validation tracked in Phase 0 spikes).

## Context

llama-server -hf gives no progress, resume or queue and blocks the request path.

## Decision

Download into the app's model library with a resumable, hash-verified downloader; launch with plain file paths.

## Consequences

Progress UI, resume, queue, bandwidth limit, pause while gaming.
