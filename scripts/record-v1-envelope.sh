#!/usr/bin/env bash
# Re-record crates/metsuke-wire/tests/fixtures/recordings/v1-envelope.hex: the
# sealed bytes a build that only knew schema v1 produced. It is compiled from
# that revision's own envelope.rs in a scratch crate, so the recording carries
# no code from the working tree.
#
# Usage: scripts/record-v1-envelope.sh [revision]
set -euo pipefail

revision="${1:-$(sed -n 's/^v1 recorded from //p' \
  "$(dirname "$0")/../crates/metsuke-wire/tests/fixtures/recordings/README.md")}"
repository="$(cd "$(dirname "$0")/.." && pwd)"
out="$repository/crates/metsuke-wire/tests/fixtures/recordings/v1-envelope.hex"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/src"

# The dependency set metsuke-wire's own manifest names, minus what envelope.rs
# does not use.
cat >"$scratch/Cargo.toml" <<'EOF'
[package]
name = "v1-cassette"
version = "0.0.0"
edition = "2024"

[dependencies]
bech32 = "0.12.0"
blake2 = "0.10.6"
ed25519-dalek = "3.0.0"
serde = { version = "1.0.229", features = ["derive"] }
serde_json = { version = "1.0.151", features = ["float_roundtrip"] }
thiserror = "2.0.20"
time = { version = "0.3.55", features = ["serde-well-known"] }
zstd = "0.13.3"
EOF

git -C "$repository" show "$revision:crates/metsuke-wire/src/envelope.rs" \
  >"$scratch/src/envelope.rs"

# The values the fixture holds. Every optional field is set, so a v2 build that
# drops or renames one cannot open the recording unchanged.
cat >"$scratch/src/main.rs" <<'EOF'
#![allow(dead_code)]
mod envelope;
use envelope::*;
use time::OffsetDateTime;

fn main() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let at = OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap();
    let env = Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: PoolId::from_cold_key(&key.verifying_key()),
        agent_version: "0.1.0".to_string(),
        counter: 42,
        timestamp: at,
        samples: vec![Sample {
            sampled_at: at,
            block_height: Some(12_318_442),
            slot: Some(163_281_005),
            slot_in_epoch: Some(281_005),
            epoch: Some(587),
            sync_progress: Some(0.5),
            node_version: Some("11.0.1".to_string()),
            node_revision: Some("0e2b4b1a".to_string()),
            clock_offset_ms: Some(-3),
        }],
    };
    let (bytes, _) = seal(&key, &env, 0).unwrap();
    println!("{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
}
EOF

# Into the scratch first: writing the frozen fixture directly would truncate it
# before the build that has to succeed to replace it has said anything.
(cd "$scratch" && cargo run --quiet) >"$scratch/v1-envelope.hex"
mv "$scratch/v1-envelope.hex" "$out"
echo "recorded $(wc -c <"$out") hex characters from $revision into $out"
