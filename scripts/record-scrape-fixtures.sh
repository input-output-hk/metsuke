#!/usr/bin/env bash
#
# Re-record the PrometheusSimple cassette fixtures under
# crates/metsuke/tests/fixtures/recordings/ from a real Leios node: a single
# forging node on the proto-devnet genesis, scraped over loopback. When to
# run and what to commit with the output: tests/fixtures/README.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
recordings="$repo/crates/metsuke/tests/fixtures/recordings"
metrics_port=12798
metrics_url="http://127.0.0.1:$metrics_port/metrics"

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null || {
      echo "error: $cmd not found on PATH" >&2
      exit 1
    }
  done
}
need nix jq curl

# The Leios source pinned in flake.nix, and the patched cardano-node rev its
# own lock file pins — the same binary the proto-devnet demo runs.
leios_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$repo/flake.lock")
leios_src=$(nix flake prefetch --json "github:input-output-hk/ouroboros-leios/$leios_rev" | jq -r .storePath)
node_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$leios_src/flake.lock")

echo "leios: $leios_rev"
echo "cardano-node: $node_rev"

workdir=$(mktemp -d /tmp/metsuke-record.XXXXXX)
node_out="$workdir/cardano-node"
nix build "github:intersectmbo/cardano-node/$node_rev#cardano-node" \
  --accept-flake-config -o "$node_out"

devnet="$leios_src/demo/proto-devnet"

# Genesis with a fresh start time, so the node forges from slot 0 now. The
# node config addresses genesis files as ./<era>-genesis.json, so they sit
# next to config.json.
cp "$devnet/config/genesis/"*.json "$workdir/"
chmod u+w "$workdir/"*.json
start_epoch=$(date +%s)
start_iso=$(date -u -d "@$start_epoch" +"%Y-%m-%dT%H:%M:%SZ")
jq --argjson time "$start_epoch" '.startTime = $time' \
  "$devnet/config/genesis/byron-genesis.json" >"$workdir/byron-genesis.json"
jq --arg time "$start_iso" '.systemStart = $time' \
  "$devnet/config/genesis/shelley-genesis.json" >"$workdir/shelley-genesis.json"

# The demo's node config with only a loopback PrometheusSimple backend.
# Converted to JSON (cardano-node's YAML parser reads JSON) so plain jq can
# address the empty-string TraceOptions key.
nix shell nixpkgs#yq-go --command yq -o=json . "$devnet/config/config.yaml" |
  jq --arg backend "PrometheusSimple 127.0.0.1 $metrics_port" \
    '.TraceOptionNodeName = "recorder" | .TraceOptions."".backends = [$backend]' \
    >"$workdir/config.json"

# No peers: a lone pool forges its own leader slots, which is all the
# recording needs.
jq '.' "$devnet/config/topology.template.json" >"$workdir/topology.json"

cp -r "$devnet/config/pools-keys/pool1" "$workdir/keys"
chmod u+w -R "$workdir/keys"
chmod 400 "$workdir/keys"/*.skey

# cwd is $workdir: the config's relative leios.db lands there, not in the repo.
cd "$workdir"
"$node_out/bin/cardano-node" run \
  --config "$workdir/config.json" \
  --topology "$workdir/topology.json" \
  --database-path "$workdir/db" \
  --socket-path "$workdir/node.socket" \
  --host-addr 127.0.0.1 \
  --port 3001 \
  --shelley-vrf-key "$workdir/keys/vrf.skey" \
  --shelley-kes-key "$workdir/keys/kes.skey" \
  --shelley-bls-key "$workdir/keys/bls.skey" \
  --shelley-operational-certificate "$workdir/keys/opcert.cert" \
  >"$workdir/node.log" 2>&1 &
node_pid=$!
trap 'kill "$node_pid" 2>/dev/null || true' EXIT

# First successful scrape: the endpoint is up but chain metrics have not been
# emitted yet — the real "metric missing" body.
startup=""
for _ in $(seq 60); do
  if startup=$(curl -sf "$metrics_url"); then
    break
  fi
  kill -0 "$node_pid" || {
    echo "error: cardano-node exited, see $workdir/node.log" >&2
    exit 1
  }
  sleep 2
done
[ -n "$startup" ] || {
  echo "error: no scrape within 120s, see $workdir/node.log" >&2
  exit 1
}
if grep -q cardano_node_metrics_blockNum_int <<<"$startup"; then
  echo "warning: chain metrics already present at first scrape; keeping the old startup fixture" >&2
else
  printf '%s' "$startup" >"$recordings/leios-node-startup.prom"
  echo "recorded: leios-node-startup.prom"
fi

# Forged chain: wait until the chain metrics appear, then a few more blocks
# so height, slot, and epoch carry distinct non-zero values.
for _ in $(seq 120); do
  body=$(curl -sf "$metrics_url" || true)
  height=$(sed -n 's/^cardano_node_metrics_blockNum_int //p' <<<"$body")
  if [ -n "$height" ] && [ "$height" -ge 5 ]; then
    printf '%s' "$body" >"$recordings/leios-node.prom"
    echo "recorded: leios-node.prom"
    break
  fi
  sleep 5
done
[ -n "${height:-}" ] && [ "$height" -ge 5 ] || {
  echo "error: chain metrics never reached height 5, see $workdir/node.log" >&2
  exit 1
}

echo
echo "expected values for tests/scrape.rs:"
grep -E '^cardano_node_metrics_(blockNum|slotNum|slotInEpoch|epoch)_int ' \
  "$recordings/leios-node.prom"
grep -oE 'version="[^"]*"|revision="[^"]*"' "$recordings/leios-node.prom" | head -2
