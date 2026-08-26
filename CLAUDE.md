# metsuke

Telemetry for the MusashiNet rewards program: `crates/metsuke` (SPO agent) samples
cardano-node's Prometheus endpoint and uploads signed batches; `crates/metsuke-server`
verifies and archives them to S3; `crates/metsuke-wire` is what the two agree on, and
neither depends on the other. Security is the top constraint — least privilege,
smallest attack surface.

## Invariants

Each is an accepted decision; read the ADR before working near it.

- Wire signature is raw Ed25519 over the request body as sent; no COSE/CBOR on the submission path — docs/adr/0001
- The counter lives in the signed header frame and counts one agent's batches; verify before decompress — docs/adr/0002
- The agent signs with the key its pool id hashes from; what the server accepts is still cold key or Calidus — docs/adr/0003
- Client SQLite spool is the only durability layer; ACK means the S3 PUT succeeded — docs/adr/0004
- S3 stores the raw signed bytes; server SQLite is a rebuildable index — docs/adr/0005
- Client and server versions are independent; the update nudge is embedded at server build — docs/adr/0006
- The agent touches only the loopback Prometheus endpoint: no socket, no journal, no groups — docs/adr/0007
- Calidus registrations are witness-checked in the server against raw db-sync metadata, never taken from an indexer — docs/adr/0008
- The server reads db-sync over the Postgres wire protocol with bound parameters, not through `psql` — docs/adr/0009

## Conventions

- Limits (sizes, rates, intervals) are configuration, never constants.
- Research notes live under docs/research/.
- $CARGO_TARGET_DIR is set, look for dependencies there.
- Coverage is `cargo llvm-cov --workspace` in the devShell; read it as a floor — binary-spawning tests record nothing, metsuke-jfb.6 has the measurement. NixOS tests (`hydraJobs.units`, nix/e2e-test.nix) are outside it.
