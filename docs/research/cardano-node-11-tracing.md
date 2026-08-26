# cardano-node 11 tracing: data sources for a lightweight SPO-side agent

This file answers musashi-ping-sak.1: how does an agent get block height, slot, epoch,
and node version from a cardano-node 11 block producer WITHOUT running cardano-tracer.
Covers the new trace-dispatcher system, built-in EKG/Prometheus metrics, the local
node-to-client socket, and version discovery, then gives a recommendation.

## 1. Trace output to stdout/journald (trace-dispatcher)

Node 10.x/11 ships the "new tracing" system (`trace-dispatcher`, replacing the legacy
`iohk-monitoring` backend). Enabled via `UseTraceDispatcher: true` in the node config.
As of mid-2026 (post-11.0.1), the legacy iohk-monitoring backend was removed entirely
(~11,000 LOC across 15 modules deleted), so on current 11.x new tracing is no longer
optional — legacy config keys (`mapBackends`, `hasPrometheus`, etc. from `iohk-monitoring`)
no longer apply.

Config structure: the four per-facet lists the upstream docs show (`TraceOptionSeverity`,
`TraceOptionDetail`, `TraceOptionLimiter`, `TraceOptionBackend`) are not what the Leios
prototype's node reads. It reads one `TraceOptions` map keyed by namespace prefix, the
empty key being the root, with `backends`, `severity`, `detail` and `maxFrequency` as
that key's fields. The proto-devnet demo config is the worked example, and the node
echoes the map it actually parsed back out as `Reflection.TracerConfigInfo`, so a
config's real effect is observable rather than inferred. Namespaces carry no `Node.`
prefix: the root is `ChainDB`, not `Node.ChainDB`. A namespace the running node has no
tracer for is reported at startup as `Reflection.TracerConsistencyWarnings` with
severity Warning and otherwise ignored — the demo config carries several.

`Stdout MachineFormat` emits one JSON object per line to stdout (captured by journald
under systemd), except for the first line, which is the Haskell `show` of
`NodeConfiguration` printed before the tracing system is up. Field shapes as observed:
crates/metsuke/tests/fixtures/recordings/leios-node-traces*.log, recorded by
scripts/record-trace-fixtures.sh. `data.kind` is the per-message discriminator and does
not always match the last segment of `ns`.

The whole trace path depends on that backend and on no other `Stdout` one:
`TraceLine::parse` takes a JSON object, so a node writing `Stdout HumanFormatColoured`
yields lines every rule skips, with nothing reporting it. The instructions page says so at
the step that sets backends, because that is where an operator merging into an existing
config can end up with the wrong one.

Fallback: if `TraceOptions` aren't fully specified, the node falls back to a hard-coded
default in `Cardano.Node.Tracing.DefaultTraceConfig`.

Severity is why an SPO cannot get the Leios events by adding a backend alone. Every
`Consensus.LeiosKernel.*` and `Consensus.LeiosPeer.*` tracer is Info except
`BlockPointMissing` (Warning) and `DbException` (Error) — the `MetaTrace` instances in
`cardano-node/src/Cardano/Node/Tracing/Tracers/Consensus.hs`. Info is below the demo
config's root threshold of Notice, so the demo's per-namespace `severity: Debug`
overrides are the only reason any of them reach a backend at all: a 40s run with those
four keys deleted and nothing else changed emitted zero `Consensus.Leios*` lines, and
the same config with them emits `Consensus.LeiosKernel.Msg` at startup on an idle node.
As a threshold, `Info` admits every measurement the program asked for. `Debug` is only
needed for the `LeiosNotify.Remote` / `LeiosFetch.Remote` wire-level namespaces, which
nobody asked for and which are the loudest. `maxFrequency: 0` means no
rate limit; omitting it caps the stream silently. Block adoption is two namespaces, not
one: `ChainDB.AddBlockEvent.AddedToCurrentChain` is Notice, while
`Forge.Loop.AdoptedBlock` is Info in the recordings and needs the same override the Leios
keys do.

`AddedToCurrentChain` reaches a backend under a root threshold of Notice or lower, so it
needs no threshold override to work. The instructions page gives it an entry anyway
(metsuke-jfb.19): a root threshold is the operator's to set, and the page has no way to
know theirs. An explicit entry for every namespace the node must emit makes the snippet
correct under any root, and the entry buys the limiter as well as the threshold — the
root sets no `maxFrequency`.

Two different configs are in play, and everything measured above is the first:

- `demo/proto-devnet/config/config.yaml` in the ouroboros-leios repo, pinned at
  `prototype-2026w32`. The fixture recordings and the e2e node both come from it
  (nix/e2e-test.nix). It is a demo, not what anyone operates.
- `book.world.dev.cardano.org/environments-pre/leios/config.json`, which is what SPOs
  run. Root threshold Notice; `Consensus.LeiosKernel` and `Consensus.LeiosPeer` at Debug
  with `maxFrequency: 0`; `ChainDB` and `Forge.Loop` at Info, which is what carries
  `AddedToCurrentChain` and `AdoptedBlock`. So an operator on this config already emits
  every namespace the agent selects. The instructions page's trace step is not a no-op
  for them even so: merging replaces a key's whole entry, so its `"severity": "Info"`
  raises `Consensus.LeiosKernel` and `Consensus.LeiosPeer` from Debug to Info. That
  admits every measurement and drops the wire-level Debug chatter above, so it costs
  nothing anyone asked for. Pasting over their `TraceOptions` rather than merging is the
  real loss — the rate limits it sets on `LeiosNotify.Remote` and `LeiosFetch.Remote`
  would go with it, which is why both snippets are framed as keys to merge. Nothing in
  this repo is verified against this config; that gap is metsuke-jfb.20.

`cardano-node trace-documentation` is not a source for these: it emits 649 namespaces
and no Leios one among them, because the Leios tracers have `documentFor _ = Nothing`.
The running node and those two `MetaTrace` instances are the only enumerations.

The eight severity names and their order are `SeverityS` in
`trace-dispatcher/src/Cardano/Logging/Types.hs` (read at cardano-node 10.1.4). Only
Debug, Info, Notice and Warning occur in the recordings.

11.0.1 specific changes: `TRACE_DISPATCHER_LOGGING_HOSTNAME` env var added (parity with
legacy `CARDANO_NODE_LOGGING_HOSTNAME`); `PrometheusSimple` backend robustness improved
(auto-restart on crash, start/stop traces, eager dangling-socket reaping); the
`peersFromNodeKernel` metric, `NodeKernelPeers` tracer, and `TraceOptionPeerFrequency`
config key were removed.

Sources:
- https://github.com/intersectmbo/cardano-node/blob/master/trace-dispatcher/doc/trace-dispatcher.md
- https://github.com/IntersectMBO/cardano-node/blob/2af1826042d9f73513884df903aa7d9fe69b8e1d/doc/New%20Tracing%20Quickstart.md
- https://developers.cardano.org/docs/get-started/infrastructure/node/new-tracing-system/new-tracing-system/
- https://github.com/IntersectMBO/cardano-node/releases/tag/11.0.1
- https://updates.cardano.intersectmbo.org/2026-05-29-performance-and-tracing/ (legacy backend removal, hermod-tracing-api/core split)

## 2. Built-in Prometheus/EKG metrics without cardano-tracer

Two backends emit metrics directly from the node process, no cardano-tracer needed:

- **EKGBackend**: exposes an EKG web/JSON endpoint on the node itself (`asMetrics` in
  the `LogFormatting` typeclass — metrics bypass severity filtering, always emitted
  once EKGBackend is configured).
- **PrometheusSimple**: a node-built-in Prometheus text-exposition endpoint, intended
  specifically for "scrape the node process itself" without cardano-tracer as
  aggregator. Docs explicitly warn: enable it only if you intend to scrape the node
  directly, to avoid opening an unnecessary port. Supports a `nosuffix` variant for
  legacy-compatible metric names.

Metric naming has two variants controlled by `TraceOptionMetricsPrefix`:
- Full-suffix (default): e.g. `cardano_node_metrics_epoch_int`, matching the official
  Grafana dashboard.
- No-suffix (`nosuffix`): e.g. `cardano_node_metrics_epoch`, matching legacy
  iohk-monitoring names for easier dashboard/alert migration.

Legacy (pre-new-tracing, or nosuffix-equivalent) metric names carrying the requested
data:
- `cardano_node_ChainDB_metrics_blockNum_int` — block height
- `cardano_node_ChainDB_metrics_slotNum_int` — absolute slot
- `cardano_node_ChainDB_metrics_slotInEpoch_int` — slot within epoch
- `cardano_node_ChainDB_metrics_epoch_int` — epoch number

No node-version metric was found in the crawled docs — metrics cover chain state and
resource usage, not the running node's version string.

Sources:
- https://developers.cardano.org/docs/get-started/infrastructure/node/new-tracing-system/metrics-migration/ (fetch 404'd directly; content summarized from search index)
- https://forum.cardano.org/t/documentation-for-ekg-prometheus-output/84652
- https://github.com/input-output-hk/cardano-tutorials/blob/master/node-setup/093_prometheus.md

## 3. Node-to-client local socket (LocalStateQuery)

`cardano-cli query tip` (and any LocalStateQuery client) connects over the node's Unix
domain socket (`CARDANO_NODE_SOCKET_PATH`), same socket block producers already expose
for `cardano-cli`/`cardano-submit-api` use. Output on recent (Conway-era) node/cli
gives exactly what's needed in one call:

```json
{
  "block": 11142430,
  "epoch": 574,
  "era": "Conway",
  "hash": "a9e4413a38aaec6ef89f8a687a58acd01a7e73675d79e9f418f6c41d2e2a7b53",
  "slot": 49630712,
  "syncProgress": "100.00"
}
```

Node version is not part of this response (LocalStateQuery is chain-state only — see
section 4).

Permission model: the socket is a plain Unix file; access is gated by filesystem
permissions/ownership, no separate auth layer. Node and client must be protocol-
compatible — a cardano-cli/cardano-node version mismatch produces decode errors
("DecoderFailure ... local state query") rather than silently wrong data, so an agent
pinning a specific cardano-cli build must track node upgrades.

Cost on a busy block producer: no quantified benchmark found in the crawled sources.
Historical fix in 1.31.0 addressed dropped connections during `query tip`, implying the
query path is lightweight enough to be treated as routine (docs recommend it as a
normal sync-check operation) but there's no measured CPU/latency number for the current
11.x LocalStateQuery path on a loaded block producer — treat this as an open question if
a hard SLO is needed, don't assume a number.

Sources:
- https://github.com/IntersectMBO/cardano-node/issues/2720
- https://forum.cardano.org/t/query-tip-error-with-9-x/134541
- https://github.com/IntersectMBO/cardano-node/releases/tag/1.31.0

## 4. Node version discovery

No metric or LocalStateQuery response carries the running node's version string in the
sources reviewed. Practical options, none requiring cardano-tracer:
- `cardano-node --version` / `cardano-cli --version` run against the binary directly
  (process-level, not a query over the socket) — output includes version, git rev, GHC
  version when built properly (a stripped/cabal-installed binary can lose the git rev).
- Node-to-client protocol version negotiation surfaces indirectly: a version mismatch
  between cardano-cli and a running node produces a decode/negotiation error naming the
  expected protocol version, but this is a failure signal, not a clean version query.

No dedicated "node version" trace message or metric was found — if this matters,
tracking the binary's own `--version` output (or the systemd unit's recorded binary
path/build) is the only source identified here.

Sources:
- https://github.com/IntersectMBO/cardano-node/issues/2720
- (general node/cli usage) https://developers.cardano.org/docs/operators/node/running-cardano/

## Recommendation

For an agent on a systemd/journald SPO box, without touching cardano-tracer:

1. **Primary source: node-built-in `PrometheusSimple` (or `EKGBackend`) endpoint.**
   Gives block height/slot/epoch as a scrape, no socket permission concerns, cheap
   (metrics bypass severity filtering and are meant for exactly this kind of direct
   scrape). Requires an **SPO node-config change**: add `PrometheusSimple` (or
   `EKGBackend`) to `TraceOptionBackend` and set `UseTraceDispatcher: true` if not
   already on (mandatory on current 11.x since legacy backend removal). Bind the
   listener to localhost to avoid opening a port unnecessarily, per the docs' own
   warning.

2. **Secondary/cross-check source: LocalStateQuery via the existing socket**
   (`cardano-cli query tip` or a direct ouroboros-network client). No node-config
   change needed — reuses the socket already present for block-producer key delivery
   tooling. Gives block/epoch/slot plus era and sync progress in one JSON response.
   Requires matching cardano-cli/cardano-node versions and read access to the socket
   file. Treat as the fallback or verification path, not primary, since per-call
   overhead on a loaded BP isn't quantified in available docs.

3. **Node version**: no in-band source exists. Read it from the binary
   (`cardano-node --version`) or from the systemd unit / package manifest that pins the
   binary — this is unavoidably out-of-band from tracing/metrics/socket.

4. **journald stdout tracing (`Stdout MachineFormat`)** is not recommended as the
   primary source here: it requires parsing an unconfirmed JSON schema per trace
   message and reacting to specific namespaces (e.g. `AddedToCurrentChain`) rather than
   pulling a point-in-time value, which is a worse fit than a metrics scrape or a
   LocalStateQuery call for "what is the current height/slot/epoch" style polling.

**Config changes needed on the SPO side:** only option 1 (metrics) requires touching
node config (`TraceOptionBackend` + `UseTraceDispatcher`). Option 2 (socket query)
needs no node-config change, only socket read access for the agent's user/group.
