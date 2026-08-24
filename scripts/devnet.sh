#!/usr/bin/env bash
#
# Bring up a local Leios devnet -- node, Postgres, db-sync -- and submit
# transaction metadata to it. What it carries and what it does not:
# docs/research/leios-devnet.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
flake="$repo/devnet"
work="$flake/.devnet"

# The leios and db-sync flakes both publish cache.iog.io in their own
# nixConfig, which --accept-flake-config is what picks up. Without it the
# Haskell builds are hours rather than minutes.
nix_args=(--accept-flake-config)

# process-compose's control socket, scoped to this devnet's work directory. Its
# default is a TCP port shared by every process-compose on the machine, and
# reaching that one would be reaching into somebody else's stack.
export PC_SOCKET_PATH="$work/process-compose.sock"

# File arguments are resolved against the caller's directory, before the cd
# below moves out of it.
args=("$@")
for i in "${!args[@]}"; do
  [ -f "${args[i]}" ] && args[i]="$(realpath "${args[i]}")"
done
set -- "${args[@]}"

# Every process in the stack addresses its state as ./.devnet, so the whole
# stack is run from the flake directory and from nowhere else.
cd "$flake"

usage() {
  cat >&2 <<EOF
usage: ${0##*/} <command>

  up                             wipe state and start node, Postgres and db-sync
  down                           stop the stack, keeping its state for inspection
  status                         where the sockets are, and whether the tip moves
  revisions                      the leios and db-sync revisions this stack runs
  submit-metadata <label> <file> submit a metadata blob on a self-send
  register-pool <file>           re-register pool1 with metadata on the certificate
  psql [args...]                 psql as the read-only role

The devnet's own k is not the network's; see docs/research/leios-devnet.md
before recording anything that reads k from a genesis file.
EOF
  exit 2
}

case "${1:-}" in
up)
  # A re-stamped genesis invalidates every byte of the last run: the node db is
  # a chain that no longer exists and db-sync's ledger state indexes it.
  rm -rf "$work"
  mkdir -p "$work"
  # process-compose opens /dev/tty for its TUI, which fails outright when
  # stdout is a pipe or a log file.
  [ -t 1 ] || export PC_DISABLE_TUI=1
  exec nix run "${nix_args[@]}" "$flake#devnet" -- --use-uds
  ;;
down)
  [ -S "$PC_SOCKET_PATH" ] || {
    echo "devnet: no stack listening at $PC_SOCKET_PATH" >&2
    exit 1
  }
  # Ask the supervisor to stop its own processes, in reverse dependency order.
  # Signalling process-compose instead would skip Postgres' shutdown and can
  # leave the data directory unclean.
  nix run "${nix_args[@]}" "$flake#devnet" -- down --use-uds
  for _ in $(seq 30); do
    [ -S "$PC_SOCKET_PATH" ] || break
    sleep 1
  done
  # State stays: the node log, Postgres data and db-sync ledger state are the
  # only evidence of whatever went wrong. `up` wipes before it starts.
  echo "devnet: stopped, state kept in $work"
  ;;
status)
  [ -d "$work" ] || {
    echo "devnet: not up ($work absent)" >&2
    exit 1
  }
  echo "node socket:     $work/node.socket"
  echo "postgres socket: $work/postgres"
  CARDANO_NODE_SOCKET_PATH="$work/node.socket" \
    CARDANO_NODE_NETWORK_ID=164 \
    nix run "${nix_args[@]}" "$flake#cardano-cli" -- latest query tip
  ;;
revisions)
  exec nix run "${nix_args[@]}" "$flake#devnet-revisions"
  ;;
submit-metadata)
  shift
  [ $# -eq 2 ] || usage
  exec nix run "${nix_args[@]}" "$flake#devnet-submit-metadata" -- "$1" "$2"
  ;;
register-pool)
  shift
  [ $# -eq 1 ] || usage
  exec nix run "${nix_args[@]}" "$flake#devnet-register-pool" -- "$1"
  ;;
psql)
  shift
  exec psql -h "$work/postgres" -U metsuke_ro -d cexplorer "$@"
  ;;
*)
  usage
  ;;
esac
