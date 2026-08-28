#!/usr/bin/env bash
#
# Re-record the PrometheusSimple cassette fixtures under
# crates/metsuke/tests/fixtures/recordings/ from a relay on the public Leios
# testnet, in the two node states whose metric sets differ. When to run and
# what to commit with the output: crates/metsuke/tests/fixtures/README.md.
#
# A relay against a real chain with real peers is the point: a metric absent
# from one of these bodies is evidence the node did not emit it, not an
# artefact of a devnet that structurally cannot reach the state.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
recordings="$repo/crates/metsuke/tests/fixtures/recordings"

# How far the first run syncs before it is restarted. The replay body only
# exists while the node is rebuilding its ledger from a snapshot older than
# its chain, so the wider that gap the wider the window to scrape it in.
SYNC_BLOCKS="${SYNC_BLOCKS:-20000}"
PORT="${PORT:-3010}"
# The port the testnet config's PrometheusSimple backend already names, so
# moving the endpoint means editing that config, not this line.
metrics_url="http://127.0.0.1:12798/metrics"

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
# own lock file pins, the same binary the testnet relay package runs.
leios_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$repo/flake.lock")
leios_src=$(nix flake prefetch --json "github:input-output-hk/ouroboros-leios/$leios_rev" | jq -r .storePath)
node_rev=$(jq -r '.nodes."cardano-node-leios".locked.rev' "$leios_src/flake.lock")

echo "leios: $leios_rev"
echo "cardano-node: $node_rev"

workdir=$(mktemp -d "${RECORD_WORKDIR:-/tmp}/metsuke-record.XXXXXX")
node_out="$workdir/cardano-node"
nix build "github:intersectmbo/cardano-node/$node_rev#cardano-node" \
  --accept-flake-config -o "$node_out"

# The relay's own scripts, made writable so its pin-config.sh can write the
# config dir it addresses relative to itself.
cp -r "$leios_src/testnet" "$workdir/testnet"
chmod -R u+w "$workdir/testnet"

# Refreshed from the deployed network, not taken as pinned. The snapshot in
# the tag is of whichever testnet was deployed when it was cut; against a
# rolled network every peer's headers fail validation at slot 0
# (NoCounterForKeyHashOCERT) and no chain metric is ever emitted. The node
# binary is pinned and the chain is not, so a re-recording is of whatever the
# network was that day.
"$workdir/testnet/pin-config.sh"

node_dir="$workdir/node"

# Job control in a script, so each background job leads its own process group.
# run-node.sh runs cardano-node in a pipeline under itself; signalling the
# script alone leaves the node holding the database lock, and the restart
# below then dies on that lock while the first node keeps writing the same
# log, a failure that reads as a healthy run.
set -m

# leios-testnet-relay's entrypoint starts a TUI under process-compose and dies
# without a tty; run-node.sh underneath it is the node on its own. It appends
# to node.log, so one file spans both runs.
start_node() {
  CARDANO_NODE="$node_out/bin/cardano-node" \
    SOURCE_DIR="$workdir/testnet" \
    WORKING_DIR="$node_dir" \
    PORT="$PORT" \
    "$workdir/testnet/run-node.sh" >"$workdir/runner.log" 2>&1 &
  node_pid=$!
}
stop_node() {
  [ -n "${node_pid:-}" ] || return 0
  kill -- -"$node_pid" 2>/dev/null || true
  wait "$node_pid" 2>/dev/null || true
  # The endpoint answering is the node still holding its ports and its
  # database; a restart that races it fails on the lock.
  local deadline=$((SECONDS + 60))
  while curl -sf -m 3 "$metrics_url" >/dev/null 2>&1; do
    [ "$SECONDS" -lt "$deadline" ] || {
      echo "error: the endpoint still answers 60s after the kill" >&2
      exit 1
    }
    sleep 1
  done
}
trap 'stop_node' EXIT

# Metric present in the body, by name. The endpoint emits no HELP and only
# some TYPE lines, so a name match has to be anchored to the start of a line.
has_metric() { grep -q "^$1[ {]" <<<"$2"; }

# Lines stating a metric, which is what tests/scrape.rs counts: a here-string
# adds the trailing newline the body does not carry, so counting comments out
# alone reports one metric too many.
count_metrics() { grep -cvE '^#|^[[:space:]]*$'; }

# Scrape until $2 says the body is the wanted one, or $1 seconds pass. Prints
# the body it stopped on. Runs under `$( )`, where `exit` would only leave the
# subshell and let the caller report a timeout that did not happen: 1 is the
# timeout, 2 is the relay dying under it, and the caller separates them.
scrape_until() {
  local deadline=$((SECONDS + $1)) predicate=$2 body
  while [ "$SECONDS" -lt "$deadline" ]; do
    body=$(curl -sf -m 3 "$metrics_url" || true)
    if [ -n "$body" ] && "$predicate" "$body"; then
      printf '%s' "$body"
      return 0
    fi
    kill -0 "$node_pid" 2>/dev/null || return 2
    sleep 1
  done
  return 1
}

# The message for whichever way scrape_until gave up, so the specific reason
# is the only one printed.
gave_up() {
  if [ "$1" -eq 2 ]; then
    echo "error: the relay exited, see $node_dir/node.log" >&2
  else
    echo "error: $2, see $node_dir/node.log" >&2
  fi
  exit 1
}

echo
echo "== bootstrapping: syncing from the bootstrap peers =="
start_node

# The threshold body, which is also what leaves the restart below something to
# replay. Not "the first body before any chain metric": the node adopts its
# first block about a second after the tracing system comes up, so against the
# live network that body is a race.
bootstrapping() {
  local height
  height=$(sed -n 's/^cardano_node_metrics_blockNum_int //p' <<<"$1")
  [ -n "$height" ] && [ "$height" -ge "$SYNC_BLOCKS" ]
}
echo "syncing to block $SYNC_BLOCKS"
bootstrap=$(scrape_until 3600 bootstrapping) ||
  gave_up $? "never reached block $SYNC_BLOCKS"
# A caught-up node is a third state, and its body is not what this cassette
# claims to be.
if grep -q 'CaughtUp' "$node_dir/node.log"; then
  echo "error: caught up before block $SYNC_BLOCKS; raise SYNC_BLOCKS" >&2
  exit 1
fi
printf '%s' "$bootstrap" >"$recordings/leios-testnet-relay-bootstrap.prom"
echo "recorded: leios-testnet-relay-bootstrap.prom ($(count_metrics <<<"$bootstrap") metrics," \
  "block $(sed -n 's/^cardano_node_metrics_blockNum_int //p' <<<"$bootstrap"))"

echo
echo "== replay: restarted onto the database it just built =="
stop_node
start_node

# Ledger replay is the only state that emits blockReplayProgress, and it ends
# when the node catches up to its own chain.
replaying() { has_metric cardano_node_metrics_blockReplayProgress_real "$1"; }
replay=$(scrape_until 300 replaying) ||
  gave_up $? "blockReplayProgress never appeared within 300s"
printf '%s' "$replay" >"$recordings/leios-testnet-relay-replay.prom"
echo "recorded: leios-testnet-relay-replay.prom ($(count_metrics <<<"$replay") metrics)"

echo
# tests/scrape.rs pins no value out of these two bodies. It asserts which
# metrics each state has, and the counts it derives. The literals it does pin
# come from the hand-captured block producer, which this script never touches.
echo "recorded $(count_metrics <"$recordings/leios-testnet-relay-bootstrap.prom") metrics" \
  "bootstrapping, $(count_metrics <"$recordings/leios-testnet-relay-replay.prom") replaying"
