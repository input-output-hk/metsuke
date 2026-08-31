# metsuke

Telemetry for the MusashiNet rewards program: `crates/metsuke` (SPO agent) scrapes
cardano-node's Prometheus endpoint and uploads signed submissions; `crates/metsuke-server`
verifies and archives them to S3; `crates/metsuke-fetch` is the developer's tool that
pulls the archive back down; `crates/metsuke-wire` is what they agree on, and it is the
only one any of them links. `metsuke-fetch`'s tests are the exception: they pull from
the real server, so that crate dev-depends on it. Security is the top constraint:
least privilege, smallest attack surface.

Whose a submission is comes from the key that signed it, and only a **Cold Key**
answers on its own. `metsuke_wire::envelope::PoolId::from_cold_key` is the one
derivation; `Attestation::attributes` and `SubmissionKey::attributes` lift it
over the received pair and the held key, and both return nothing under a
**Leios Key**. See CONTEXT.md for both keys. Every site that decides whose a
submission is starts there —
`metsuke::identity::check_pool_id` at agent startup,
`metsuke_server::authority::Attributed::decode` per upload, and
`metsuke_fetch::sync::checked` per downloaded object — and each says in its own
words what it does with the absence.

## Invariants

Each is an accepted decision; read the ADR before working near it.

- Wire signature is raw Ed25519 over the request body as sent; no COSE/CBOR on the submission path. See docs/adr/0001.
- Client SQLite spool is the only durability layer; ACK means the S3 PUT succeeded. See docs/adr/0004.
- S3 stores the raw signed bytes and is the only store; the server holds no state. See docs/adr/0005.
- Client and server versions are independent; the update nudge is embedded at server build. See docs/adr/0006.
- A submission signed by a pool's Leios key claims its pool in a header, and that claim is believed only where a roster file lists that key for that pool; the cold-key path derives it still. See docs/adr/0011.
- Without `[log]` the agent touches only the loopback Prometheus endpoint: no socket, no journal, no groups. `[log].source` picks what it reads: the pipe holds no group either, and only the journal costs `SupplementaryGroups=systemd-journal`, which reads every unit's journal. See docs/adr/0010 and nix/unit.nix.

## Conventions

- Limits (sizes, rates, intervals) are configuration, never constants.
- Research notes live under docs/research/.
- $CARGO_TARGET_DIR is set, look for dependencies there.
- Coverage is `cargo llvm-cov --workspace` in the devShell; read it as a floor, because binary-spawning tests record nothing. metsuke-jfb.6 has the measurement. NixOS tests (`hydraJobs.units`, nix/e2e-test.nix) are outside it.

## Tests

@justfile
