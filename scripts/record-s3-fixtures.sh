#!/usr/bin/env bash
#
# Re-record the S3 cassette against a single-node Garage on loopback. What the
# recordings hold, when to re-record and what to commit with the output:
# crates/metsuke-server/tests/fixtures/README.md.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
recordings="$repo/crates/metsuke-server/tests/fixtures/recordings/s3"

api_port=${API_PORT:-3900}
rpc_port=${RPC_PORT:-3901}
bucket=metsuke-record
region=garage
# Throwaway: this cluster lives for one run of this script. Garage's own
# format for an imported key, which is what lets the credentials exist before
# the cluster does.
access_key_id=GK1122334455667788990011aa
secret_access_key=0011223344556677889900112233445566778899001122334455667788990011
rpc_secret=5c1915fa04d0b6739675c61bf5907eb0fe3d9c69850c83820f51b4d25d13868c
outsider_access_key_id=GK00112233445566778899aabb
outsider_secret_access_key=aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899

need() {
  for cmd in "$@"; do
    command -v "$cmd" >/dev/null || {
      echo "error: $cmd not found on PATH" >&2
      exit 1
    }
  done
}
need nix cargo

# --inputs-from: the nixpkgs this flake is locked to, so the endpoint a
# recording names is the one the repo builds against.
garage_out=$(nix build --inputs-from "$repo" --no-link --print-out-paths nixpkgs#garage)
garage="$garage_out/bin/garage"
"$garage" --version

workdir=$(mktemp -d /tmp/metsuke-record-s3.XXXXXX)
cat >"$workdir/garage.toml" <<EOF
metadata_dir = "$workdir/meta"
data_dir = "$workdir/data"
db_engine = "sqlite"
replication_factor = 1
rpc_bind_addr = "127.0.0.1:$rpc_port"
rpc_public_addr = "127.0.0.1:$rpc_port"
rpc_secret = "$rpc_secret"

[s3_api]
s3_region = "$region"
api_bind_addr = "127.0.0.1:$api_port"
EOF
export GARAGE_CONFIG_FILE="$workdir/garage.toml"

"$garage" server >"$workdir/garage.log" 2>&1 &
garage_pid=$!
trap 'kill "$garage_pid" 2>/dev/null || true' EXIT

ready=""
for _ in $(seq 60); do
  if "$garage" status >/dev/null 2>&1; then
    ready=yes
    break
  fi
  kill -0 "$garage_pid" || {
    echo "error: garage exited, see $workdir/garage.log" >&2
    exit 1
  }
  sleep 1
done
[ -n "$ready" ] || {
  echo "error: garage never answered within 60s, see $workdir/garage.log" >&2
  exit 1
}

# Cluster setup says nothing the recording depends on, so it goes to the log
# with the server's own output.
{
  node_id=$("$garage" node id -q | cut -d@ -f1)
  "$garage" layout assign -z record -c 1G "$node_id"
  "$garage" layout apply --version 1
  "$garage" bucket create "$bucket"
  "$garage" key import --yes -n recorder "$access_key_id" "$secret_access_key"
  "$garage" bucket allow --read --write "$bucket" --key "$access_key_id"

  # A second key, allowed nothing: what the endpoint answers a presigned
  # request it will not serve.
  "$garage" key import --yes -n outsider \
    "$outsider_access_key_id" "$outsider_secret_access_key"
} >>"$workdir/garage.log" 2>&1

mkdir -p "$recordings"
ENDPOINT="http://127.0.0.1:$api_port" \
  BUCKET="$bucket" \
  REGION="$region" \
  ACCESS_KEY_ID="$access_key_id" \
  SECRET_ACCESS_KEY="$secret_access_key" \
  OUTSIDER_ACCESS_KEY_ID="$outsider_access_key_id" \
  OUTSIDER_SECRET_ACCESS_KEY="$outsider_secret_access_key" \
  RECORDINGS="$recordings" \
  ENDPOINT_VERSION="$(basename "$garage_out" | cut -d- -f2-)" \
  cargo run --quiet --package metsuke-server --example record-s3

echo
echo "recorded into $recordings:"
ls -1 "$recordings"
