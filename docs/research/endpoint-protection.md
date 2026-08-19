# Endpoint protection design space

metsuke's upload endpoint receives interval metric uploads from SPO agents, payloads signed
with the pool's on-chain-registered Calidus key (CIP-0088v2, Ed25519). This surveys options for
replay protection, registration gating, DoS resistance, transport, and abuse handling, plus prior
art from comparable collector endpoints. No decision is made here; musashi-ping-sak.9 picks from
these options.

## 1. Replay protection

Payload signatures alone don't prove freshness — a captured, validly-signed upload can be
resent. Three mechanisms, combinable:

- **Timestamp window.** Client includes a signed timestamp; server rejects if outside a tolerance
  (commonly 30s–5min, balancing clock skew against replay window size). Cheap, stateless, but
  allows replay *within* the window. [Source: replay-attack API pentest and design writeups.]
- **Per-key monotonic counter / nonce.** Server tracks last-accepted sequence number per Calidus
  key; rejects non-increasing values. Closes the in-window replay gap completely but requires
  server-side state per key (one integer, cheap at Musashi's scale — hundreds to low thousands of
  pools, not millions of end users).
- **Idempotency keys (Stripe pattern).** Client sends a unique key per logical operation; server
  caches the outcome and returns it verbatim on retry, rather than reprocessing. Standard for
  mutating POST endpoints; scoped per-operation, not per-transport-request; typical TTL ~24h.
  [Source: Stripe idempotency docs, brandur.org Postgres implementation.]

**Trade-off for an interval-upload protocol:** uploads happen on a fixed cadence (e.g. every N
minutes), so a per-key monotonic counter is a natural fit — the client already knows "this is
upload #k" or can use the sample's own timestamp/slot as the counter, giving replay protection for
free without a separate nonce store. Idempotency keys matter less here than for payment APIs
since duplicate accepted uploads are idempotent-by-nature (same slot/height data) rather than
side-effecting money movement — but a *dedup* semantic (silently drop duplicate valid uploads
instead of erroring) is still useful for client-side retry safety. A timestamp window is cheap
insurance to add regardless, catching stale/misconfigured clients before the counter check runs.

## 2. Registration gating

- **Closed allowlist of participant pool IDs.** Musashi maintains its own list of onboarded
  pools (e.g. a config file, database table, or the metric-schema decision's storage layer);
  reject uploads from Calidus keys not on the list even if the on-chain registration is valid.
  Matches "invite-only" testing/rollout phases.
- **Open to any registered pool.** Trust CIP-0088v2's on-chain registration as sufficient —
  any pool with a valid Calidus key registration can upload. Simpler operationally, no
  separate allowlist to maintain, but widens the anonymous-uploader set to the whole
  Cardano SPO population before Musashi has vetted them.
- **Where an allowlist lives:** either in the same store as pool→Calidus-key mappings (if
  the server already indexes on-chain registrations, per musashi-ping-sak.2's findings), or
  as a lightweight separate participant table keyed by pool ID / Calidus pubkey. A hybrid is
  common in practice: verify on-chain registration validity first (proves the key belongs to a
  real pool), then check an additional Musashi-specific allowlist (proves this pool opted into
  this specific data collection).

**Recommendation for consideration:** given the epic's stated bias toward least privilege and
smallest attack surface (and eventual mainnet block-producer exposure), start with a closed
allowlist during the pilot; the on-chain-registration check is necessary but not sufficient for
"should ingest this pool's data."

## 3. Cost asymmetry / DoS on signature verification

Ed25519 verification is fast (~70k verifications/sec on commodity hardware, far cheaper than
secp256k1's ~1,300/sec) but still 1000x+ costlier than a size check, so cheap pre-checks should gate
before verification runs. [Source: Ed25519 performance figures from patent literature/ecosystem
docs.]

Ordered pre-checks before invoking Ed25519 verify:
1. **Request size cap** — reject oversized bodies at the HTTP layer (e.g. reverse proxy /
   framework body-limit) before any parsing.
2. **Structural/schema validation** — cheap parse of the envelope (pool ID present, signature
   field present, size sane) before touching the signature.
3. **Per-key / per-IP rate limit** — token bucket keyed on claimed pool ID (or source IP as a
   coarser fallback) rejects floods before verification; this is the standard defense pattern
   for public signed-upload endpoints (Prometheus remote-write receivers document 429 vs 5xx
   semantics precisely for this reason — 429 tells senders "back off, don't retry the whole
   batch"; 5xx implies "retry, we may not have persisted it"). [Source: Prometheus Remote-Write
   2.0 spec.]
4. **Registration/allowlist check** (§2) — a cheap lookup that rejects unknown pool IDs before
   the expensive Ed25519 verify runs at all.
5. **Ed25519 verification** — only for requests that pass 1–4.

An optional "registration token" (a lightweight bearer credential issued at onboarding,
independent of the Calidus key) could let the allowlist check in step 4 double as authentication,
avoiding a lookup keyed on an unverified claimed pool ID — but this reintroduces a second secret
to manage. Given musashi-ping-sak.2 already establishes Calidus-key verification machinery, reusing
pool-ID-based allowlist lookup (no separate token) is the simpler option unless step 4's lookup
itself becomes a targeted cost (e.g. IP-address rate limiting as a backstop handles that).

## 4. Transport

- **TLS is a hard requirement regardless of payload signing.** Payload signatures give
  authenticity and integrity of the signed fields, but not confidentiality, and don't protect
  metadata (source IP, timing, endpoint being hit) from network observers, nor prevent
  connection-level DoS/tampering below the application layer. The Prometheus remote-write spec
  explicitly treats transport auth as a separate layer from the payload format for this reason.
  [Source: Prometheus Remote-Write spec.]
- **mTLS vs bearer token vs pure payload signature** — these aren't mutually exclusive; modern
  practice combines a transport-layer credential with an application-layer one. Options:
  - *mTLS only*: strongest, but requires PKI issuance/rotation infrastructure per pool — heavier
    than Musashi likely wants for a first rollout (blockperf takes this route, see §6).
  - *Bearer token + payload signature*: token identifies "this pool is registered with Musashi"
    (redundant with the allowlist check in §2, so may be the same artifact), payload signature
    proves the specific upload's authenticity/integrity from the Calidus key. Simpler to operate
    (standard TLS certs on the server only), token rotation is centrally managed.
  - *Payload signature only, over plain TLS*: relies entirely on Calidus-key verification for
    both identity and integrity; simplest for clients (no extra credential to manage beyond the
    key they already have), but loses the cheap allowlist short-circuit a token would give for
    free at the TLS/HTTP layer.
  [Source: general API auth method comparisons, 2026.]

**For this system**, since every client already holds a Calidus key and the payload signature is
mandated by the collection design, plain server-side TLS (not mTLS) plus payload signature is the
minimal-complexity option; a bearer/registration token is an optional addition if the allowlist
check needs to happen before parsing the payload (see §3 step 4).

## 5. Abuse handling

- **Per-pool rate limits**, keyed on pool ID/Calidus pubkey once verified (or claimed pool ID
  pre-verification, per §3): caps how often a legitimate pool can upload, defends against a
  compromised or misbehaving client hammering the endpoint.
- **Quarantine of misbehaving keys**: if a key's uploads fail validation repeatedly, or a
  monotonic counter regresses (§1) in a way that suggests key compromise or misconfiguration,
  temporarily suspend ingestion for that key and surface an operator alert rather than silently
  dropping or silently accepting.
- **Monitoring signals**: rejection rate by cause (bad signature, unknown pool, stale timestamp,
  counter regression, rate-limited), upload cadence drift, and per-pool upload success rate feed
  directly into deciding when to quarantine.

## 6. Prior art

| System | Auth mechanism | Notes |
|---|---|---|
| Prometheus remote-write receivers | Transport-layer: Basic auth / bearer token + tenant header + TLS; treats auth as out of protocol scope | Formalizes 429 (back off, sender's fault) vs 5xx (retry, receiver's fault) semantics for rate limiting. [prometheus.io/docs/specs/prw/remote_write_spec_2_0](https://prometheus.io/docs/specs/prw/remote_write_spec_2_0/) |
| blockperf (Cardano Foundation, MQTT via AWS IoT Core) | X.509 client certificates issued by CF, per-client identity baked into cert (mTLS-equivalent) | Operators contact CF to get a cert; cert's client-identifier scopes which topic they can publish to — allowlist is implicit in cert issuance. [github.com/cardano-foundation/blockperf](https://github.com/cardano-foundation/blockperf) |
| Polkadot/Substrate telemetry | None — any node can push to a public shard over WebSocket; "security" achieved by choosing a private telemetry URL instead of the public one | No cryptographic gatekeeping in the open-source telemetry backend; access control is "don't tell the URL to untrusted nodes," which is not usable as-is for a spec explicitly biased toward least privilege. [github.com/paritytech/substrate-telemetry](https://github.com/paritytech/substrate-telemetry) |

Musashi-ping's Calidus-key-signed design is already stronger than Polkadot telemetry's model and
comparable in spirit to blockperf's cert-per-client approach, but achieves the per-client identity
via an on-chain-verifiable key instead of a CA-issued certificate — avoiding blockperf's manual
"contact CF for a cert" onboarding step, at the cost of needing the pre-verify pre-checks in §3
since anyone can present *a* signature, whereas blockperf's TLS handshake rejects unregistered
clients before any application data is even read.

## Sources

- [Prometheus Remote-Write 2.0 specification](https://prometheus.io/docs/specs/prw/remote_write_spec_2_0/)
- [Prometheus Remote-Write 1.0 specification](https://prometheus.io/docs/specs/prw/remote_write_spec/)
- [blockperf GitHub repository](https://github.com/cardano-foundation/blockperf)
- [substrate-telemetry GitHub repository](https://github.com/paritytech/substrate-telemetry)
- [Polkadot node management docs](https://w3f.github.io/polkadot-wiki-mkdocs/build/build-node-management/)
- [Stripe: Designing robust and predictable APIs with idempotency](https://stripe.com/blog/idempotency)
- [Implementing Stripe-like Idempotency Keys in Postgres — brandur.org](https://brandur.org/idempotency-keys)
- [Cube Exchange: What is a Replay Attack?](https://www.cube.exchange/what-is/replay-attack)
- [Understanding HTTP message signatures: A developer's guide](https://victoronsoftware.com/posts/http-message-signatures/)
- [CIP-0151: On-Chain Registration - Stake Pools](https://cips.cardano.org/cip/CIP-0151)
- [Blockfrost: Calidus Pool Keys — Secure SPO Interaction on Cardano](https://blog.blockfrost.io/calidus-pool-keys/)
- [gitmachtl/cardano-signer README](https://github.com/gitmachtl/cardano-signer/blob/main/README.md)
- [API Authentication Methods Compared — DevWithTools](https://devwithtools.org/blog/api-authentication-methods)
- [Zuplo: Top 7 API Authentication Methods Compared](https://zuplo.com/learning-center/top-7-api-authentication-methods-compared)
- Ed25519 verification throughput figures: patent literature and ecosystem write-ups citing ~70k verifications/sec vs secp256k1's ~1,300/sec (reasoning aggregated from search results, not a single primary source — verify against the ed25519.cr.yp.to paper if the exact figure matters for capacity planning).
