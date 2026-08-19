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
  the Leios db-sync (label 867, highest nonce wins; all-zero key rejects).

Calidus lookups are cached forever. On a signature-verify failure the server does
one fresh lookup and retries before rejecting, rate-limited per pool.

## Consequences

- The client is trivial: it signs with whatever key it is configured with.
- Cache-forever looks wrong but is correct: rotation heals on the first
  post-rotation upload via the refresh-on-fail path, without TTL churn against
  db-sync.
- The cold-key path keeps the ingest runtime db-sync-free; only Calidus pools
  depend on the db-sync host.
