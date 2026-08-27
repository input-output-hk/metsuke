# metsuke

Telemetry for the MusashiNet rewards program: `crates/metsuke` (SPO agent) samples
cardano-node's Prometheus endpoint and uploads signed batches; `crates/metsuke-server`
verifies and archives them to S3; `crates/metsuke-wire` is what the two agree on, and
neither depends on the other. Security is the top constraint — least privilege,
smallest attack surface.

Whose a submission is is derived, never claimed — CONTEXT.md, **Cold Key**.
The two ends that enforce it are `metsuke::identity::check_pool_id` and
`metsuke_server::authority::Signed::pool_id`.

## Invariants

Each is an accepted decision; read the ADR before working near it.

- Wire signature is raw Ed25519 over the request body as sent; no COSE/CBOR on the submission path — docs/adr/0001
- Client SQLite spool is the only durability layer; ACK means the S3 PUT succeeded — docs/adr/0004
- S3 stores the raw signed bytes and is the only store; the server holds no state — docs/adr/0005
- Client and server versions are independent; the update nudge is embedded at server build — docs/adr/0006
- Without `[log]` the agent touches only the loopback Prometheus endpoint: no socket, no journal, no groups. `[log].source` picks what it reads: the pipe holds no group either, and only the journal costs `SupplementaryGroups=systemd-journal`, which reads every unit's journal — docs/adr/0010, nix/unit.nix

## Conventions

- Limits (sizes, rates, intervals) are configuration, never constants.
- Research notes live under docs/research/.
- $CARGO_TARGET_DIR is set, look for dependencies there.
- Coverage is `cargo llvm-cov --workspace` in the devShell; read it as a floor — binary-spawning tests record nothing, metsuke-jfb.6 has the measurement. NixOS tests (`hydraJobs.units`, nix/e2e-test.nix) are outside it.
