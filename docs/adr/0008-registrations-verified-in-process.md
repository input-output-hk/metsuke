# 8. Calidus registrations are verified in the server, from raw chain metadata

Status: accepted (2026-08-24)

## Context

A CIP-151 registration is transaction metadata: anyone who pays for a
transaction can post one naming any pool, and only the cold-key witness inside
it separates an operator's registration from a stranger's claim about their
pool. Indexers that offer a pre-validated view exist, but trusting one would
put a third party inside an authorization decision, and the workspace already
holds every primitive the check needs but a CBOR codec.

## Decision

The server reads raw label-867 metadata from the Leios db-sync over a read-only
role and runs the CIP-151 check itself: blake2b-224 of the witness key must
equal the pool id in the payload scope. It runs on the db-sync host, so the
connection is loopback.

This is the only place CBOR enters the server, and ADR 1's ban does not reach
it: nothing here touches the submission envelope, and re-encoding the payload
to hash it fails a signature when our encoder and the signer's disagree rather
than passing one — the direction that makes the recomputed hash bind the exact
fields we decoded.

## Consequences

- Verification is ours, so a bug in it is ours; the primitives are already in
  the workspace and a CBOR codec is the only addition.
- A registration counts only once it is k blocks deep — the node's immutable
  tip, reconstructed against a table that carries no such flag. A rollback can
  only ever retract authority, so the wait buys little against a forged grant;
  it is there because Leios reorgs are the less predictable half and a
  registration that appears and vanishes is worse to reason about than one that
  arrives late. The price is paid by revocation, which now takes up to k plus
  the resolution lifetime to reach a running server — pick that lifetime
  against k, not against db-sync load, which is loopback and negligible.
- k is read from the network's Shelley genesis, whose path is configuration. It
  is a genesis parameter, reachable from no `cardano-cli query` subcommand and
  absent from the protocol parameters, and it differs between the network we
  record fixtures on and the one we serve. A hand-written copy would disagree
  with the network silently, in the direction that weakens the wait above.
- A Postgres client brings an async runtime into a server that had none.
- Colocation is a deployment constraint, not a protocol one: a separate host
  reaching db-sync over some authenticated transport would serve equally.
