# 3. Key-agnostic verification: cold key or Calidus, server decides

Status: accepted (2026-08-19)

## Context

SPOs differ on whether the pool cold key may touch the telemetry box. Calidus
(CIP-151 on CIP-88, metadata label 867) provides a hot Ed25519 key witnessed by
the cold key, resolvable from chain data, with nonce-based rotation and an
all-zero-key revocation convention. Mandating either key type would exclude one
camp; the choice is operator policy, not protocol.

## Decision

Every upload carries (pool_id, vkey, signature). The server decides whether vkey
may speak for pool_id, trying both paths:

- Cold key: blake2b-224(vkey) equals the pool id (the pool id *is* that hash).
  Pure computation, no infrastructure.
- Calidus: vkey equals the pool's registered Calidus key, resolved by SQL against
  the Leios db-sync (label 867, highest nonce wins; an all-zero key, or two
  different keys sharing the highest nonce, resolve to no key).

Calidus lookups are cached forever. A vkey the cached registration cannot explain
triggers one fresh lookup and a re-check before rejecting, rate-limited per pool.
That is the refresh trigger — not a signature failure, which says nothing about
the registered key, since the signature is checked against the vkey the upload
itself presents. A refresh the budget refuses decided nothing, so it is not a
rejection.

## Consequences

- The client is trivial: it signs with whatever key it is configured with.
- Cache-forever looks wrong but is correct for rotation: it heals on the first
  post-rotation upload via the refresh-on-fail path, without TTL churn against
  db-sync. It does not heal in the other direction. A revocation, or a rotation
  away from a key someone else now holds, leaves the cached key verifying: the
  holder of the old key never presents anything the cache cannot explain, and
  nothing in the legitimate flow forces the refresh that would replace it
  (metsuke-4zo.44).
- The cold-key path keeps the ingest runtime db-sync-free; only Calidus pools
  depend on the db-sync host.
