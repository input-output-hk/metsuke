# 2. Replay protection inside the signed payload, verify before decompress

Status: accepted (2026-08-19)

## Context

Signed uploads can be captured and resent. Conventional designs put anti-replay
material (nonce, date) in signed headers, which reintroduces the canonicalization
problem ADR 1 avoids. Putting it inside the body means the server cannot check the
counter until it has decompressed — so the processing order decides whether
unauthenticated bytes ever reach the decompressor.

## Decision

A per-pool monotonic counter and a timestamp live inside the signed JSON payload.
The server processes each upload cheap-to-expensive:

size cap → allowlist → per-pool rate limit → Ed25519 verify (over the compressed
bytes) → bounded decompression → replay check (counter must increase, timestamp
within window) → accept.

Counter state (`pool_id → last_counter, last_seen`) lives in server SQLite and is
updated only after the full chain passes. Size and rate limits are configuration,
not constants.

## Consequences

- Only authenticated bytes are decompressed; a signature check is the only work an
  unknown sender can cause past the allowlist.
- A replayed valid message costs one bounded decompress before rejection,
  rate-limited per pool.
- Each archived object carries its own replay evidence inside the signed bytes.
- The timestamp is a backstop: if counter state is lost, it bounds the replay
  window while counters re-seed from the archive (ADR 5).
