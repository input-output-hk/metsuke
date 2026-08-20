# 5. Archive stores the raw signed bytes; SQLite is a rebuildable index

Status: accepted (2026-08-19), amended (2026-08-20)

## Context

Received batches could be stored decompressed, normalized, or split into
analytics-friendly rows. Every transform is lossy against the one property the
signature gives us: the stored bytes are provably what the pool sent.

## Decision

Each accepted submission becomes one S3 object holding the zstd bytes exactly as
received, keyed `v1/<pool_id>/<date>/<timestamp>-<counter>.json.zst`. Envelope
metadata (signature, key, counter, schema version) rides along as object metadata
headers, so an object carries everything its own verification needs. SQLite
indexes the bucket for whatever has to be queried — replay counters first,
per-submission rows once an endpoint reads them — and the bucket alone is the
source of truth.

## Consequences

- The bucket is a self-verifying corpus: object bytes + metadata headers reproduce
  the original verification without any database.
- The SQLite index is disposable — rebuildable from a bucket listing, and replay
  counters re-seed from each pool's latest object metadata after state loss.
- Consumers decompress locally; there is no server-side decompressed copy.
  Dashboard-friendly formats are a downstream concern, derived from the archive.
