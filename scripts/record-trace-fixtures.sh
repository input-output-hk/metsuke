#!/usr/bin/env bash
#
# Re-record the `Stdout MachineFormat` cassette fixture under
# crates/metsuke/tests/fixtures/recordings/ from a real Leios node. When to run
# and what to commit with the output: crates/metsuke/tests/fixtures/README.md.
#
# Three forging nodes, because every Leios namespace the fixture exists for is
# about a peer: an announcement received, an EB body fetched from somebody, a
# vote counted towards a quorum. A lone node produces none of them.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
recordings="$repo/crates/metsuke/tests/fixtures/recordings"

# A forger emits an EB only from the transactions that did NOT fit in its
# ranking block (ouroboros-consensus NodeKernel/Forge.hs, partitionMempool), so
# the recording needs a mempool larger than one RB. These are the shape of that
# overflow, not properties of the protocol: enough near-maximum-size
# transactions to overrun maxBlockBodySize several times over.
FLOOD_TXS="${FLOOD_TXS:-60}"
FLOOD_FEE="${FLOOD_FEE:-2000000}"
FLOOD_METADATA_STRINGS="${FLOOD_METADATA_STRINGS:-200}"
# Seconds before the flood, and the capture window after it.
WARMUP_SECONDS="${WARMUP_SECONDS:-30}"
CAPTURE_SECONDS="${CAPTURE_SECONDS:-300}"

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null || {
      echo "error: $cmd not found on PATH" >&2
      exit 1
    }
  done
}
need nix jq

# The Leios source pinned in flake.nix, and the patched cardano-node rev its
# own lock file pins, the same binary the proto-devnet demo runs. cardano-cli
# comes from the same rev: only the patched build can address the Dijkstra era
# this devnet forges in, a stock cardano-cli of any version dies with
# "TODO Dijkstra: shelleyBasedEraConstraints: era not supported".
leios_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$repo/flake.lock")
leios_src=$(nix flake prefetch --json "github:input-output-hk/ouroboros-leios/$leios_rev" | jq -r .storePath)
node_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$leios_src/flake.lock")

echo "leios: $leios_rev"
echo "cardano-node: $node_rev"

workdir=$(mktemp -d /tmp/metsuke-trace-record.XXXXXX)
nix build "github:intersectmbo/cardano-node/$node_rev#cardano-node" \
  --accept-flake-config -o "$workdir/cardano-node"
nix build "github:intersectmbo/cardano-node/$node_rev#cardano-cli" \
  --accept-flake-config -o "$workdir/cardano-cli"
node="$workdir/cardano-node/bin/cardano-node"
cli="$workdir/cardano-cli/bin/cardano-cli"

config="$leios_src/demo/proto-devnet/config"

# Genesis with a fresh start time, so the nodes forge from slot 0 now.
cp -r "$config/genesis" "$workdir/genesis"
chmod u+w -R "$workdir/genesis"
start_epoch=$(date +%s)
start_iso=$(date -u -d "@$start_epoch" +"%Y-%m-%dT%H:%M:%SZ")
jq --argjson time "$start_epoch" '.startTime = $time' \
  "$config/genesis/byron-genesis.json" >"$workdir/genesis/byron-genesis.json"
jq --arg time "$start_iso" '.systemStart = $time' \
  "$config/genesis/shelley-genesis.json" >"$workdir/genesis/shelley-genesis.json"

# Each node on its own 127/8 address: --host-addr is also the source address
# for outbound connections, so three nodes sharing 127.0.0.1 collide on the
# listener's 4-tuple (demo/proto-devnet/run.sh says the same).
for i in 1 2 3; do
  dir="$workdir/node$i"
  mkdir -p "$dir"

  # The demo config with stdout as the only backend: this fixture is the line
  # stream, and PrometheusSimple would only open a port nothing here reads.
  # Converted to JSON (cardano-node's YAML parser reads JSON) so plain jq can
  # address the empty-string TraceOptions key.
  nix shell nixpkgs#yq-go --command yq -o=json . "$config/config.yaml" |
    jq --arg n "node$i" \
      '.TraceOptionNodeName = $n | .TraceOptions."".backends = ["Stdout MachineFormat"]' \
      >"$dir/config.json"

  access_points=$(for j in 1 2 3; do
    [ "$i" -ne "$j" ] && echo "{\"port\": 300$j, \"address\": \"127.2.0.$j\"}"
  done | jq -s '.')
  jq --argjson accessPoints "$access_points" \
    '.localRoots[0].accessPoints = $accessPoints' \
    "$config/topology.template.json" >"$dir/topology.json"

  for era in byron shelley alonzo conway dijkstra; do
    ln -s "../genesis/$era-genesis.json" "$dir/"
  done

  cp -r "$config/pools-keys/pool$i" "$dir/keys"
  chmod u+w -R "$dir/keys"
  chmod 400 "$dir/keys"/*.skey
done

pids=()
for i in 1 2 3; do
  (
    # cwd is the node's own directory: the config's relative leios.db lands
    # there, one per node, not in the repo and not shared.
    cd "$workdir/node$i"
    exec "$node" run \
      --config config.json \
      --topology topology.json \
      --database-path db \
      --socket-path node.socket \
      --host-addr "127.2.0.$i" \
      --port "300$i" \
      --shelley-vrf-key keys/vrf.skey \
      --shelley-kes-key keys/kes.skey \
      --shelley-bls-key keys/bls.skey \
      --shelley-operational-certificate keys/opcert.cert \
      >"$workdir/node$i.stdout" 2>"$workdir/node$i.stderr"
  ) &
  pids+=($!)
done
trap 'kill "${pids[@]}" 2>/dev/null || true' EXIT

export CARDANO_NODE_SOCKET_PATH="$workdir/node1/node.socket"
export CARDANO_NODE_NETWORK_ID=164

for _ in $(seq "$WARMUP_SECONDS"); do
  "$cli" latest query tip >/dev/null 2>&1 && break
  sleep 1
done
"$cli" latest query tip >/dev/null || {
  echo "error: node1 never answered a tip query, see $workdir/node1.stderr" >&2
  exit 1
}
sleep "$WARMUP_SECONDS"

# A chain of self-sends, each near maxTxSize. Chained by txid rather than by
# re-querying: a utxo query reads the ledger at the tip and knows nothing of
# the mempool, so every submission in a batch would pick the same input.
addr=$("$cli" latest address build \
  --payment-verification-key-file "$config/utxo-keys/utxo1/utxo.vkey")
jq -n --argjson n "$FLOOD_METADATA_STRINGS" \
  '{"674": {"msg": [range($n) | "x"*64]}}' >"$workdir/metadata.json"

utxo=$("$cli" latest query utxo --address "$addr" --output-json)
txin=$(jq -er 'to_entries | max_by(.value.value.lovelace) | .key' <<<"$utxo")
value=$(jq -er 'to_entries | max_by(.value.value.lovelace) | .value.value.lovelace' <<<"$utxo")

# Built first, submitted second: a build/sign/txid round trip costs three
# cardano-cli startups, and the mempool has to hold them all at once.
for i in $(seq 1 "$FLOOD_TXS"); do
  "$cli" latest transaction build-raw \
    --tx-in "$txin" \
    --tx-out "$addr+$((value - i * FLOOD_FEE))" \
    --fee "$FLOOD_FEE" \
    --metadata-json-file "$workdir/metadata.json" \
    --out-file "$workdir/tx$i.raw"
  "$cli" latest transaction sign \
    --tx-body-file "$workdir/tx$i.raw" \
    --signing-key-file "$config/utxo-keys/utxo1/utxo.skey" \
    --testnet-magic 164 \
    --out-file "$workdir/tx$i.signed"
  txid=$("$cli" latest transaction txid --tx-file "$workdir/tx$i.signed" | jq -er .txhash)
  txin="$txid#0"
done
for i in $(seq 1 "$FLOOD_TXS"); do
  "$cli" latest transaction submit --tx-file "$workdir/tx$i.signed" >/dev/null
done
echo "flooded $FLOOD_TXS transactions, capturing for ${CAPTURE_SECONDS}s"

sleep "$CAPTURE_SECONDS"
kill "${pids[@]}" 2>/dev/null || true
wait || true

# The node that both forged an EB and received one: the announce/vote/certify
# path only shows both ends on a node that took both. Whether that is node1,
# node2 or node3 is up to three VRFs.
capture=$(for i in 1 2 3; do
  kinds=$(grep -o '"ns":"Consensus.Leios[^"]*"' "$workdir/node$i.stdout" | sort -u | wc -l)
  echo "$kinds $workdir/node$i.stdout"
done | sort -rn | head -1 | cut -d' ' -f2)
grep -q '"ns":"Consensus.LeiosKernel.BlockForged"' "$capture" &&
  grep -q '"ns":"Consensus.LeiosPeer.Announcement"' "$capture" || {
  echo "error: no node both forged and received an EB, see $workdir/node*.stdout" >&2
  exit 1
}

# The line number a window ends or starts at, named so a stream that never
# emitted the trace it is cut at says which edge is missing. An empty number
# reaches sed as an open-ended range instead.
window_edge() {
  local edge=$1 pick=$2 pattern=$3 matches found
  matches=$(grep -n "$pattern" "$capture" || true)
  if [[ $pick == first ]]; then
    found=$(echo "$matches" | head -1 | cut -d: -f1)
  else
    found=$(echo "$matches" | tail -1 | cut -d: -f1)
  fi
  [[ -n $found ]] || {
    echo "error: $edge: no line matching $pattern in $capture" >&2
    exit 1
  }
  echo "$found"
}

# Two contiguous slices of that one stream, addressed by line number: nothing
# inside either window is dropped or reordered, so both are still recordings of
# what the node said. The whole run is mostly Forge.Loop.Call, a volume
# measurement rather than a fixture; this script prints it at the end.
#
# Startup, through the first Leios line. Carries the one line on the stream
# that is not JSON (cardano-node prints its NodeConfiguration before the
# tracing system is up) and the Reflection.* traces that report which tracers
# the config actually enabled.
startup_end=$(window_edge "the startup window's end" first '"ns":"Consensus.LeiosKernel.Msg"')
sed -n "1,${startup_end}p" "$capture" >"$recordings/leios-node-traces-startup.log"
echo "recorded: leios-node-traces-startup.log"

# The Leios round: from the first EB forged or announced to the last Leios
# line. Unfiltered within the window. What the fixture is for is exercising
# metsuke's own namespace and severity selection against everything the node
# emits alongside the traces that are wanted.
leios_start=$(window_edge "the Leios window's start" first \
  '"ns":"Consensus.LeiosKernel.BlockForged"\|"ns":"Consensus.LeiosPeer.Announcement"')
leios_end=$(window_edge "the Leios window's end" last '"ns":"Consensus.Leios')
sed -n "${leios_start},${leios_end}p" "$capture" >"$recordings/leios-node-traces.log"
echo "recorded: leios-node-traces.log"

echo
echo "line rate over the whole capture, for spool sizing:"
lines=$(wc -l <"$capture")
bytes=$(stat -c%s "$capture")
leios=$(grep -c '"ns":"Consensus.Leios' "$capture")
echo "  $lines lines, $bytes bytes, $leios of them Consensus.Leios*"

echo
echo "namespaces in the Leios window:"
grep -o '"ns":"[^"]*"' "$recordings/leios-node-traces.log" | sort | uniq -c | sort -rn
