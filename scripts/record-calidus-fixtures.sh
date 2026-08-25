#!/usr/bin/env bash
#
# Re-record the CIP-151 registration fixtures under
# crates/metsuke-server/tests/fixtures/calidus/. What each one is and when to
# re-record: that directory's README.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$repo/crates/metsuke-server/tests/fixtures/calidus"

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null || {
      echo "error: $cmd not found on PATH" >&2
      exit 1
    }
  done
}
need nix jq b2sum od psql

signer="$(nix build --accept-flake-config --no-link --print-out-paths \
  "$repo/devnet#cardano-signer")/bin/cardano-signer"

work=$(mktemp -d /tmp/metsuke-calidus.XXXXXX)
trap 'rm -rf "$work"' EXIT

to_hex() { od -An -tx1 -v "$1" | tr -d ' \n'; }
from_hex() { printf '%b' "$(sed 's/../\\x&/g' <<<"$1")"; }

# The seeds the server suite already signs with (tests/support/mod.rs). Using
# them here is what lets one recorded registration answer both halves of an
# authority test.
cold_a=$(printf '07%.0s' $(seq 32))
cold_b=$(printf '09%.0s' $(seq 32))

# cardano-signer takes the Calidus key as a public key, so the seeds the suite
# holds have to be turned into one. Signing a byte nobody looks at is how it
# prints the public key belonging to a secret key.
pubkey_of() {
  "$signer" sign --data-hex 00 --secret-key "$1" --json | jq -er .publicKey
}
calidus_a=$(pubkey_of "$(printf '03%.0s' $(seq 32))")
calidus_b=$(pubkey_of "$(printf '05%.0s' $(seq 32))")

# CIP-151 revokes with 32 zero bytes. The second is 32 bytes that are not a
# point on the curve, which is what any metadata writer can post.
revoked=$(printf '00%.0s' $(seq 32))
not_a_key=02$(printf '00%.0s' $(seq 31))

mkdir -p "$out/recordings" "$out/crafted"

# One registration, as cardano-signer emits it: the label-867 metadata in both
# the CBOR a transaction carries and the JSON one is submitted as.
record() {
  local name=$1 cold=$2 calidus=$3 nonce=$4
  "$signer" sign --cip151 \
    --calidus-public-key "$calidus" \
    --secret-key "$cold" \
    --nonce "$nonce" \
    --json --out-file "$work/$name.json" \
    --out-cbor "$work/$name.cbor"
  to_hex "$work/$name.cbor" >"$out/recordings/$name.hex"
  echo "$name"
}

record nonce-1-key-a "$cold_a" "$calidus_a" 1
record nonce-5-key-a "$cold_a" "$calidus_a" 5
record nonce-5-key-b "$cold_a" "$calidus_b" 5
record revoked-nonce-9 "$cold_a" "$revoked" 9
record not-a-key-nonce-3 "$cold_a" "$not_a_key" 3
record other-pool-nonce-1 "$cold_b" "$calidus_a" 1

# The registration a pool cold key cannot make: the payload scopes cold key A's
# pool, and the witness is cold key B's over that payload. Every signature in
# it is real; only binding the witness key to the scope pool id refuses it.
extended=$("$signer" sign --cip151 \
  --calidus-public-key "$calidus_a" --secret-key "$cold_a" --nonce 1 --json-extended)
payload=$(jq -er .payloadCbor <<<"$extended")
payload_hash=$(jq -er .payloadHash <<<"$extended")

# `--json-extended` is the only place cardano-signer prints the pool id it
# derived from a cold key, so each scope below costs a run of its own.
pool_a=$(jq -er .poolIdHex <<<"$extended")
pool_b=$(jq -er .poolIdHex <<<"$("$signer" sign --cip151 \
  --calidus-public-key "$calidus_a" --secret-key "$cold_b" --nonce 1 --json-extended)")

forger_key=$(pubkey_of "$cold_b")
# The key hash the COSE protected header carries.
forger_hash=$(from_hex "$forger_key" | b2sum -l 224 | cut -d' ' -f1)

# COSE protected header: {1: -8 (EdDSA), "address": h'<28 bytes>'}, 41 bytes.
protected="a201276761646472657373581c${forger_hash}"
[ ${#protected} -eq 82 ] || {
  echo "error: protected header is ${#protected} hex digits, expected 82" >&2
  exit 1
}

# Sig_structure = ["Signature1", protected, external_aad, payload] (RFC 8152),
# with an empty external_aad and the payload hash as the payload.
sig_structure="846a5369676e617475726531" # array(4), "Signature1"
sig_structure="${sig_structure}5829${protected}405820${payload_hash}"
signature=$("$signer" sign --data-hex "$sig_structure" --secret-key "$cold_b" --signature-only)

cose_sign1="845829${protected}a166686173686564f45820${payload_hash}5840${signature}"
cose_key="a4010103272006215820${forger_key}"
printf '%s' "a1190363a3000201${payload}0281a201${cose_key}02${cose_sign1}" \
  >"$out/crafted/scope-mismatch.hex"
echo "scope-mismatch"

# The one round trip through a chain: everything above is what cardano-signer
# emits, and only db-sync says what a server reads back.
reader() { "$repo/scripts/devnet.sh" psql -A -t "$@"; }

# k comes from the devnet's own genesis, which is not the network's --
# docs/research/leios-devnet.md.
k=$(jq -er .securityParam "$repo/devnet/.devnet/shelley-genesis.json")
query="$repo/crates/metsuke-server/src/registrations.sql"

# Nothing is recorded from psql itself: the server binds its parameters over the
# wire protocol (ADR 0009), which psql cannot speak, so an answer taken here is
# not an answer the server asked for. PREPARE is the closest psql gets, and
# every read below goes through it so the scope filter is never re-typed.
#
# One row per line and nothing else: `--quiet` is what keeps psql's SET and
# PREPARE tags out, which every `wc -l` below counts on. `bytea_output` is set
# rather than assumed, because a role or server default of `escape` would render
# the registration as octal text and the fixture would be written from it.
prepared() {
  local scope=$1 limit=$2 depth=$3 out=$4
  {
    echo "SET bytea_output = 'hex';"
    echo "PREPARE registrations AS"
    cat "$query"
    echo "EXECUTE registrations('0x$scope', $depth, $limit);"
  } | reader --no-psqlrc --quiet -v ON_ERROR_STOP=1 --file - >"$out"
}

# Rows scoping a pool whatever their depth: at zero a row in the tip block
# counts, which is what a submission that has not waited k out looks like.
rows_for() {
  prepared "$1" "$2" 0 "$work/count"
  wc -l <"$work/count"
}

# The crowded scope sits one row past this. A cap of the recorder's own rather
# than the server's configured one: the bound is a query parameter, so any
# value it exceeds proves it holds.
crowded_cap=2

# Top a scope up to the rows the assertions below name, and refuse a devnet
# already past them: rows left by an earlier recording would make this a
# cassette of nothing in particular. Equality is what a re-run after the depth
# wait timed out looks like, so those submissions are not repeated.
fill() {
  local scope=$1 want=$2 json=$3 held
  held=$(rows_for "$scope" $((want + 1)))
  if [ "$held" -gt "$want" ]; then
    # `held` is itself bounded, so this says at least that many.
    echo "error: $held or more label-867 rows scope $scope, wanted $want; run scripts/devnet.sh up first" >&2
    exit 1
  fi
  if [ "$held" -eq "$want" ]; then
    echo "devnet: reusing the $want row(s) already scoping $scope"
    return
  fi
  for _ in $(seq $((want - held))); do
    "$repo/scripts/devnet.sh" submit-metadata 867 "$json" >/dev/null
  done
}

fill "$pool_a" 1 "$work/nonce-1-key-a.json"
fill "$pool_b" $((crowded_cap + 1)) "$work/other-pool-nonce-1.json"

# Until the rows are k deep the query is right to answer nothing, so this waits
# the depth out rather than reading an empty answer as a broken query. Both
# scopes are polled: which of them was submitted last depends on what the devnet
# already held, so neither is reliably the laggard. The crowded scope is asked
# for its *whole* count here, because a short answer under a tighter bound is
# equally what a row that has not reached depth looks like -- proving the bound
# needs every row known deep first.
#
# A psql failure mid-wait is retried rather than fatal: the submissions above
# have already landed, and a run that threw them away would wait the depth out
# again from the start.
# Set once, not per iteration: `answered` is then true of the wait rather than of
# its last attempt, so a blip on the final try still reports the counts that say
# what was actually wrong. The counts are the last ones actually read.
answered=""
rows_a=0
rows_b=0
for _ in $(seq 120); do
  prepared "$pool_a" $((crowded_cap + 1)) "$k" "$work/pool-a" || { sleep 5; continue; }
  prepared "$pool_b" $((crowded_cap + 1)) "$k" "$work/pool-b" || { sleep 5; continue; }
  answered=yes
  rows_a=$(wc -l <"$work/pool-a")
  rows_b=$(wc -l <"$work/pool-b")
  [ "$rows_a" -eq 1 ] && [ "$rows_b" -eq $((crowded_cap + 1)) ] && break
  sleep 5
done

# Three failures that read alike and are not: a db-sync that never answered, a
# chain that has not reached depth, and counts that are simply wrong. Reporting
# the first two as each other sends someone waiting on a stack that is down, or
# reading SQL over a chain that only needed longer.
[ -n "$answered" ] || {
  echo "error: db-sync did not answer; is the devnet up?" >&2
  exit 1
}
if [ "$rows_a" -ne 1 ] || [ "$rows_b" -ne $((crowded_cap + 1)) ]; then
  if [ "$rows_a" -eq 0 ] || [ "$rows_b" -lt $((crowded_cap + 1)) ]; then
    echo "error: the rows are still less than $k blocks deep" \
      "($rows_a scoping pool A, $rows_b scoping pool B)" >&2
  else
    echo "error: $rows_a rows scoping pool A and $rows_b scoping pool B," \
      "wanted 1 and $((crowded_cap + 1))" >&2
  fi
  exit 1
fi
echo "the shipped query answers on chain"

# Every row of the crowded scope is now known k deep, so an answer short of them
# under a tighter bound can only be the LIMIT cutting it.
prepared "$pool_b" "$crowded_cap" "$k" "$work/bounded"
bounded=$(wc -l <"$work/bounded")
[ "$bounded" -eq "$crowded_cap" ] || {
  echo "error: $bounded rows answered under a limit of $crowded_cap," \
    "with $rows_b of them deep" >&2
  exit 1
}
echo "the shipped query stops at its limit"

# The registration as db-sync filed it, off the shipped query's own answer, with
# the leading \x psql renders a bytea with. One row is what the wait asserted, so
# this cannot fold two of them into one meaningless blob. Matched whole rather
# than with `grep -q`, which passes on any one line matching.
hex=$(sed 's/^\\x//' <"$work/pool-a")
[[ $hex =~ ^[0-9a-f]+$ ]] || {
  echo "error: pool A's row did not read back as hex: ${hex:0:32}" >&2
  exit 1
}
printf '%s' "$hex" >"$out/recordings/on-chain-nonce-1-key-a.hex"
echo "on-chain-nonce-1-key-a"
