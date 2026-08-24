# 2. Replay protection inside the signed payload, verify before decompress

Status: accepted (2026-08-19)

## Context

Signed uploads can be captured and resent. Conventional designs put anti-replay
material (nonce, date) in signed headers, which reintroduces the canonicalization
problem ADR 1 avoids. Putting it inside the body means the server cannot check the
counter until it has decompressed — so the processing order decides whether
unauthenticated bytes ever reach the decompressor.

## Decision

A per-pool monotonic counter and a timestamp live inside the signed JSON payload,
covered by the same signature as the data. Counter state (`pool_id →
last_counter, last_seen`) lives in server SQLite.

Three things the ingest path must hold, whatever checks it grows:

- Nothing is decompressed before its signature verifies.
- Cheap checks run before expensive ones, so the work an unauthenticated sender
  can cause is bounded by the first check that rejects them.
- A counter is spent only by a submission that succeeded outright, storage
  included.

Size and rate limits are configuration, not constants.

## Consequences

- A signature check is the most an unknown sender can cost the server past the
  allowlist, unless the server runs a Calidus directory: the key check runs
  ahead of the signature and reaches the directory for any allowlisted pool it
  has not resolved yet, cold-key-only pools included, since nothing says which
  kind a pool is until the lookup comes back empty (ADR 3). What bounds it is
  not the ordering but the allowlist, the per-pool upload limit, and a ceiling
  per pool of one resolution per TTL (ADR 8).
- A replayed valid message costs one bounded decompress before rejection,
  rate-limited per pool.
- Each archived object carries its own replay evidence inside the signed bytes.
- The timestamp is a backstop: if counter state is lost, it bounds the replay
  window while counters re-seed from the archive (ADR 5).
- The check sequence is not fixed here. Adding, removing or reordering a check is
  a code change, answerable to the three invariants above and to nothing else in
  this file.
