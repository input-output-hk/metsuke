# Fixtures

Cassettes for the two things metsuke reads off a node: the PrometheusSimple
scrape (`*.prom`, replayed via wiremock in tests/scrape.rs) and the
`Stdout MachineFormat` trace stream (`*.log`).

- `recordings/` — real bodies captured from a Leios node by
  scripts/record-scrape-fixtures.sh (`*.prom`) and
  scripts/record-trace-fixtures.sh (`*.log`). Never edit by hand. Re-record on every
  bump of the `cardano-node-leios` input in flake.nix, and commit the new
  bodies together with the updated expected values in tests/scrape.rs in the
  same change. That bump moves two other recorded values, each with a check
  that goes red until it does: devnet/flake.lock's `leios` has to name the same
  revision (`checks.leios-pin`, in this flake), and nix/e2e-test.nix's `poolId`
  has to be the one the new tag's demo keys hash to (`hydraJobs.pool-id`, in
  the devnet flake).
- `recordings/leios-testnet-bp.prom` — one scrape captured by hand from a block
  producer on the Leios testnet, not by the recorder: a node with a real chain
  and real peers returns metrics the devnet recordings structurally cannot, the
  block-replay progress among them. metsuke-uxw.2 puts this cassette under the
  recorder, and until it does there is no command that re-records it.
- `edge-cases/` — hand-authored bodies for deliberate parser edge cases. No
  provenance: every value in them is invented, which is why they live apart
  from the recordings.

The two `.log` recordings are contiguous line-number windows of one node's
stdout, not the whole run: a startup window and a window bracketing one Leios
round. The recorder cuts them and says where. Nothing inside a window is
dropped or reordered, so a selector tested against one faces every trace the
node emitted between those two lines, wanted or not.
