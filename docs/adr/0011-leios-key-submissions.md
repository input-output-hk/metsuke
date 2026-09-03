# 11. Submissions signed by a pool's Leios key, resolved through a roster file

Status: accepted (2026-08-31)

## Context

[ADR 0001](0001-raw-ed25519-detached-signature.md) has one signer: the cold
key, whose hash is the pool id, so a submission names its pool by derivation
and nothing is looked up. The cost is
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

The roster is a file. A timer beside the server queries `pool-state` and writes
it; the server re-reads it when it changes and never queries anything itself.
That split is the point: the query needs a node socket and a chain client, and
the thing on the ingest path gets neither. It lists, per pool, every key the
chain currently registers and every key it has announced for the next epoch, so
a rotation is accepted before it takes effect and the epoch boundary is not an
event.

The timer is ours to run, not an operator's to write: the server is ours, so a
roster nobody regenerates is our outage. `services.metsuke-server.roster` is the
unit, and it costs the ingest host a cardano-node socket and a cardano-cli in
one unit's PATH. The generator runs as its own named user and hands the file
over by group, so what the ingest unit gains is one group and no new reach.

A new roster is put in place by rename, never written over the old one. The
server reads the file on the request path, so a writer that truncates in place
hands it half a roster; rename makes the swap one step and gives the name a new
inode, which is what the server notices the change by. Timestamps cannot carry
that on their own: successive rosters are naturally the same length, and two
written inside one mtime tick would look identical while listing different keys.

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
  and says so. A cold-key object stays self-verifying forever
  ([ADR 0005](0005-archive-raw-signed-bytes.md)).
- The agent and the server both gain a BLS implementation, against ADR 0001's
  preference for one verification path and this project's preference for the
  smallest attack surface. There is no cardano-cli command that signs with this
  key, so there was no way to borrow one.
- A stale roster refuses a pool that rotated. That is the failure we chose:
  the alternative, accepting a key the chain no longer registers, accepts
  submissions from whoever holds a retired key. How often the timer runs is
  therefore the ceiling on how long a rotation takes to land, and the only
  thing that bounds that refusal.
