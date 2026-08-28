#!/usr/bin/env bash
#
# Re-record nix/fixtures/leios-preprod-node-config.json. What this configuration
# is and why it matters: docs/research/cardano-node-11-tracing.md, section 1.
#
# Recorded rather than fetched at build time: upstream rewrites this file
# whenever the environment is redeployed, and a recording is something you can
# diff. Re-run when the environment moves, and read the diff before committing
# it. A namespace that left the file is a namespace the merge assertions in
# nix/e2e-test.nix no longer cover.
set -euo pipefail

url="https://book.world.dev.cardano.org/environments-pre/leios/config.json"

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$repo/nix/fixtures/leios-preprod-node-config.json"

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null || {
      echo "error: $cmd not found on PATH" >&2
      exit 1
    }
  done
}
need curl jq

workdir=$(mktemp -d /tmp/metsuke-preprod-config.XXXXXX)
trap 'rm -rf "$workdir"' EXIT

curl --fail --silent --show-error "$url" -o "$workdir/config.json"

# Not for reformatting -- the bytes are committed as served -- but so a page
# that answered 200 with an error document or a redirect stub fails here rather
# than at the next e2e run.
jq -e '.TraceOptions[""].backends | index("Stdout MachineFormat")' \
  "$workdir/config.json" >/dev/null || {
  echo "error: $url served no root Stdout MachineFormat backend" >&2
  exit 1
}

mkdir -p "$(dirname "$out")"
cp "$workdir/config.json" "$out"
echo "recorded: ${out#"$repo"/}"
echo
echo "tracing keys in the recording:"
jq -r '.TraceOptions | keys | length' "$out"
