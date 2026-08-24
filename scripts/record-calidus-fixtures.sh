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
  "$signer" sign --data-hex 00 --secret-key "$1" --json | jq -r .publicKey
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
payload=$(jq -r .payloadCbor <<<"$extended")
payload_hash=$(jq -r .payloadHash <<<"$extended")

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

# One registration, so the query's answer is one row. A devnet carrying rows
# from an earlier recording would answer with however many it accumulated,
# which is a cassette of nothing in particular. One is what a re-run after the
# depth wait timed out looks like, so that submission is not repeated.
already=$(reader -c "select count(*) from tx_metadata where key = 867")
case "$already" in
0) "$repo/scripts/devnet.sh" submit-metadata 867 "$work/nonce-1-key-a.json" >/dev/null ;;
1) echo "devnet: reusing the registration already on chain" ;;
*)
  echo "error: the devnet holds $already label-867 rows; run scripts/devnet.sh up first" >&2
  exit 1
  ;;
esac
for _ in $(seq 60); do
  reader -c "select encode(bytes,'hex') from tx_metadata where key = 867" >"$work/on-chain.hex"
  [ -s "$work/on-chain.hex" ] && break
  sleep 5
done
[ -s "$work/on-chain.hex" ] || {
  echo "error: db-sync filed no label-867 row" >&2
  exit 1
}
tr -d '\n' <"$work/on-chain.hex" >"$out/recordings/on-chain-nonce-1-key-a.hex"
echo "on-chain-nonce-1-key-a"

# The query the server ships, against the chain it was recorded on. k comes from
# the devnet's own genesis, which is not the network's --
# docs/research/leios-devnet.md.
pool_id=$(jq -r .poolIdHex <<<"$extended")
k=$(jq -er .securityParam "$repo/devnet/.devnet/shelley-genesis.json")
query="$repo/crates/metsuke-server/src/registrations.sql"

# Nothing is recorded from this: the server binds its parameters over the wire
# protocol (ADR 0009), which psql cannot speak, so an answer taken here is not
# an answer the server asked for. PREPARE is the closest psql gets, and running
# it is what says the shipped SQL still finds a registration on a real chain.
#
# Until the registration is k deep the query is right to answer nothing, so this
# waits the depth out rather than reading an empty answer as a broken query.
prepared() {
  {
    echo "PREPARE registrations AS"
    cat "$query"
    echo "EXECUTE registrations('0x$pool_id', $k);"
  } | reader --no-psqlrc --quiet -v ON_ERROR_STOP=1 --file -
}
for _ in $(seq 120); do
  prepared >"$work/answer" && [ -s "$work/answer" ] && break
  sleep 5
done
[ -s "$work/answer" ] || {
  echo "error: the registration is still less than $k blocks deep" >&2
  exit 1
}
echo "the shipped query answers on chain"
