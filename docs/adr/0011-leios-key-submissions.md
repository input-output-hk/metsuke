# 11. Submissions signed by a pool's Leios key, resolved through a roster file

Status: accepted (2026-08-31)

## Context

ADR 0001 has one signer: the cold key, whose hash is the pool id, so a
submission names its pool by derivation and nothing is looked up. The cost is
that every machine running an agent holds the pool's cold key. Operators are
told to copy an offline key onto live hosts, which is what decades of practice
says not to do, and the cold key cannot be rotated without re-registering the
pool.

Leios gives every pool a second registered key, a BLS12-381 key the node forges
and votes with (`spsLeiosKey.leiosPubKey` in `cardano-cli query pool-state`).
It is not an identity key: it does not hash to the pool id, so a submission it
signed cannot say whose it is without reading the chain. Its registration
carries a proof of possession the ledger has already checked, and a future node
version can rotate it, which the cold key cannot do.

## Decision

A submission may be signed by either key. The cold key path is unchanged.

Under a Leios key the submission names its pool in a header, and that name is
believed only where the signature stands under a key the **Key Roster** files
for that pool. Both checks or neither: a claimed pool with no roster entry, a
key the roster does not list for it, and a signature that does not verify are
one refusal.

The roster is a file. Something outside this repository queries `pool-state`
and writes it; the server re-reads it when it changes and never queries
anything itself. It lists, per pool, every key the chain currently registers
and every key it has announced for the next epoch, so a rotation is accepted
before it takes effect and the epoch boundary is not an event.

Leios-key submissions are signed under this project's own domain separation
tag, not the consensus one. The key is the same key that signs votes; the
domain separation is what keeps a signature made for one from being a signature
in the other.

## Consequences

- The cold key stops being required on a reporting machine, which is the whole
  point. What sits there instead is the key the node votes with, so the blast
  radius moves rather than shrinks: it is rotatable, which the cold key is not.
- A pool id on the wire is no longer redundant. It is a lookup hint, wrong
  until the signature says otherwise, and a reader who trusts it before that
  has reintroduced the thing ADR 0001 removed.
- An archived Leios-signed object is no longer verifiable on its own. Checking
  one needs the roster that was current when it was written, and the fetch tool
  keeps none, so it presents such an object's attestation without checking it
  and says so. A cold-key object stays self-verifying forever (ADR 0005).
- The agent and the server both gain a BLS implementation, against ADR 0001's
  preference for one verification path and this project's preference for the
  smallest attack surface. There is no cardano-cli command that signs with this
  key, so there was no way to borrow one.
- A stale roster refuses a pool that rotated. That is the failure we chose:
  the alternative, accepting a key the chain no longer registers, accepts
  submissions from whoever holds a retired key.
