# Fixtures

Cassettes for the two things metsuke reads off a node: the PrometheusSimple
scrape (`*.prom`, replayed via wiremock in tests/scrape.rs) and the
`Stdout MachineFormat` trace stream (`*.log`).

- `recordings/` — real bodies captured from a Leios node, never edited by
  hand. What each holds and what records it is the list below; two of them no
  longer have a recorder. Re-record what does on every bump of the
  `cardano-node-leios` input in flake.nix, and commit the new bodies with the
  updated expected values in tests/scrape.rs in the same change. That bump
  moves two other recorded values, each with a check that goes red until it
  does: devnet/flake.lock's `leios` has to name the same revision
  (`checks.leios-pin`, in this flake), and nix/e2e-test.nix's `poolId` has to
  be the one the new tag's demo keys hash to (`hydraJobs.pool-id`, in the
  devnet flake).
- `edge-cases/` — hand-authored bodies for deliberate parser edge cases. No
  provenance: every value in them is invented, which is why they live apart
  from the recordings.

Which node state each `.prom` cassette holds, because the metric set is a
function of that state and not of the node's version:

- `leios-testnet-relay-bootstrap.prom` and `leios-testnet-relay-replay.prom` —
  one relay on the public Leios testnet, syncing from its bootstrap peers and
  then restarted onto the chain database that first run built. Only the
  restart reports ledger replay. `scripts/record-scrape-fixtures.sh` records
  both in one run, and says there why it refreshes the testnet config instead
  of taking the pinned tag's snapshot.
- `leios-testnet-bp.prom` — one scrape taken by hand from a block producer on
  the same testnet, kept because a relay never forges and so no recorder
  driving one produces the forge counters this body carries. Nothing
  re-records it.
- `leios-node.prom` and `leios-node-startup.prom` — a single forging node on
  the proto-devnet genesis, from when record-scrape-fixtures.sh drove that
  devnet. Frozen: the recorder now drives the relay, so nothing re-records
  this pair — metsuke-uxw.10.

The two `.log` recordings are contiguous line-number windows of one node's
stdout, not the whole run: a startup window and a window bracketing one Leios
round. The recorder cuts them and says where. Nothing inside a window is
dropped or reordered, so a selector tested against one faces every trace the
node emitted between those two lines, wanted or not.
