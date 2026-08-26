# 1. Raw Ed25519 detached signature over the request body

Status: accepted (2026-08-19)

## Context

Uploads must be attributable to a pool via keys SPOs already hold. The Cardano
ecosystem default is COSE_Sign1 (CIP-8): cardano-signer emits it, wallets emit it.
Adopting it would pull a CBOR/COSE stack into both crates and give the wire format
two possible envelopes. Header-based signing schemes (RFC 9421, SigV4) were also
considered; they require canonicalization — choosing, ordering, and casing the
signed headers — and canonicalization mismatch between signer and verifier is a
recurring vulnerability class.

## Decision

The client signs the request body — the exact bytes sent — with a raw Ed25519
detached signature. The body is a zstd frame sequence: a skippable frame
carrying the header as plaintext JSON, then the data frame the payload is
compressed into (`envelope.rs`). One signature covers both. HTTP headers carry
pool id, verification key, and signature. No COSE, no CBOR anywhere in the
runtime data path. CIP-88/Calidus registration stays in existing SPO tooling;
the server only reads its on-chain result.

## Consequences

- One verification path, one byte string signed; no canonicalization surface.
- The stored S3 object (the same bytes) is independently verifiable forever.
- The server can verify before decompressing (see ADR 2 for the check order).
- SPOs cannot reuse wallet-produced COSE signatures; the client binary does the
  signing.
- "Runtime data path" means the submission: reading a pool's on-chain Calidus
  registration is CBOR and is not this path (ADR 8).
