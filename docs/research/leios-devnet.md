# Local Leios devnet

What the local devnet can and cannot answer, and the chain measurements behind
it. `scripts/devnet.sh` runs the stack; `devnet/flake.nix` holds the values it
applies and the reasons they sit where they do. This file does not repeat them.

## What it runs

A single forging node on the leios proto-devnet genesis, Postgres, and the
db-sync `leios1-dbsync-a-1` runs, orchestrated by process-compose. The node and
cli come from the `leios` flake input, db-sync and its schema from
`cardano-db-sync-leios`; `scripts/devnet.sh revisions` prints both, and
`devnet/flake.lock` owns them.

The node and cli substitute from `cache.iog.io`. db-sync does not — the
leios-branch `cardano-*` libraries are uncached, so a first run builds them and
takes correspondingly long.

`nix run ./devnet#cardano-signer` is the reference CIP-151 implementation,
packaged from the same lock. It is what a label-867 blob must be recorded from;
nothing in the devnet calls it.

## Security parameter

| Network | k | epochLength | slotLength |
|---|---|---|---|
| leios (what the server serves) | 108 | 21600 | 1 |
| leios proto-devnet, as shipped | 2160 | 432000 | 1 |

`activeSlotsCoeff` is 0.05 on both, and Shelley wants `epochLength = 10k/f`,
which both satisfy — as does the shrunk pair the setup script patches in. Byron
carries its own `protocolConsts.k`, equal to Shelley's on each network, and a
`slotDuration` of 20000 ms on leios against the proto-devnet's 250 ms; the Byron
era is exited at epoch 0, so only Shelley's values reach anything.

The devnet's own k is not a fact about anything. One forging node has no
competing chain, so nothing here ever rolls back and the k-deep wait cannot be
exercised locally at all. It is shrunk only to buy a short epoch, which is what
puts an announced-then-effective pool retirement within one sitting. **A fixture
recorded here carries the devnet's k, not the network's** — ADR 0008 is where the
rule about reading k from the network's genesis lives.

The real leios genesis lives in cardano-playground under
`docs/environments-pre/leios/`, which is also where the node config and
`db-sync-config.json` come from.

### Why the node config says PraosMode

The demo config ships `ConsensusMode: GenesisMode`, and a lone forger under it
stalls at exactly k blocks. Chain selection's Limit on Eagerness will not extend
the current chain beyond its LoE fragment, which is derived from the peers'
chains; with an empty topology that fragment sits at genesis, so block k+1 is
forged, logged as `StoreButDontChange`, and never adopted. The tip then stops
moving while the clock does not, and once the wall clock passes tip + 3k/f slots
the ledger view can no longer be forecast: `Forge.Loop.NoLedgerView`, forever.

Observed at k = 6: six blocks adopted, tip frozen at slot 294, first
`NoLedgerView` at slot 655 = 294 + 360. The LoE defends against adversarial
chains served by peers, and this devnet has none. Raising k only moves the
ceiling, which is why the shipped 2160 hides the problem rather than avoiding it.

### Genesis hashes

cardano-node tolerates a config with no `*GenesisHash` keys; db-sync refuses to
parse one without `ByronGenesisHash`. Byron's hash is taken over its canonical
encoding rather than over the file, so `cardano-cli hash genesis-file` answers a
different question there and the node rejects the result as a
`GenesisHashMismatch`; `cardano-cli byron genesis print-genesis-hash` is the one
that agrees with the node.

## db-sync

`devnet/db-sync-config.json` is the production file copied verbatim, with one
change: `PrometheusPort` moved off 8080 so a local run does not collide. The keys
that decide behaviour are `NetworkName`, `NodeConfigFile`, `RequiresNetworkMagic`
and `EnableFutureGenesis`; the rest is iohk-monitoring logging.

Nothing in cardano-parts or cardano-playground sets db-sync `insert_options`, so
production runs the defaults and `tx_metadata` is fully populated. A cassette
recorded under narrower options would not describe production.

## cardano-cli on this chain

The node runs Dijkstra from epoch 0 (`TestDijkstraHardForkAtEpoch: 0`), and
cardano-cli 11.1.0.0's `latest` still means Conway, so era-bearing commands are
`cardano-cli dijkstra ...`. `latest query tip` answers anyway.

Two things the leios prototype adds to a pool registration certificate:
`--bls-signing-key-file` is mandatory, and `--pool-cost` is rejected below
`minPoolCost`.

## What it cannot answer

- Rollback, depth, and anything downstream of them. One forger, no reorgs.
- Whether the real network's k is what the server should wait for. That is a
  reading of the leios genesis, not an observation here.
- Leios block internals. db-sync files transactions and metadata; nothing in this
  stack exercises the voting or EB stages beyond keeping the chain alive.
