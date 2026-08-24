# CIP-151 registration fixtures

Label-867 metadata blobs, hex, as `tx_metadata.bytes` holds them: the whole
`{867: registration}` map and not the registration alone. Re-record with
scripts/record-calidus-fixtures.sh, which also says which key signs what.

- `recordings/` — what `cardano-signer sign --cip151` emits, and one round trip
  through the local devnet's db-sync. Never edit by hand; re-record on every
  bump of the `cardano-signer-src` input in devnet/flake.nix.
- `crafted/` — assembled by the recorder out of real signatures to be a blob no
  compliant signer produces. Only the scope-mismatch case lives here.

| recording | cold key | Calidus key | nonce |
|---|---|---|---|
| `nonce-1-key-a` | `test_key` | `calidus_key` | 1 |
| `nonce-5-key-a` | `test_key` | `calidus_key` | 5 |
| `nonce-5-key-b` | `test_key` | `rotated_calidus_key` | 5 |
| `revoked-nonce-9` | `test_key` | 32 zero bytes | 9 |
| `not-a-key-nonce-3` | `test_key` | 32 bytes off the curve | 3 |
| `other-pool-nonce-1` | `other_key` | `calidus_key` | 1 |

`on-chain-nonce-1-key-a` is `nonce-1-key-a` submitted to the devnet and read
back out of db-sync; `query.csv` is what
crates/metsuke-server/src/registrations.sql printed once it was k blocks deep.
What each proves is asserted in tests/calidus.rs and tests/dbsync.rs.

`crafted/scope-mismatch` scopes `test_key`'s pool and is witnessed by
`other_key`, with `other_key`'s hash in the COSE protected header.

Re-recording starts with `scripts/devnet.sh up`, because the recorder wants a
chain holding one registration and no more. It then waits out the depth, which
looks like several minutes of nothing.
