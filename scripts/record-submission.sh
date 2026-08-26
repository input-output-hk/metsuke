#!/usr/bin/env bash
# Re-record crates/metsuke-wire/tests/fixtures/recordings/submission-*.hex: the
# wire bytes this build's `seal` produces for each payload shape. The values
# sealed are in crates/metsuke-wire/examples/record-submission.rs.
set -euo pipefail

repository="$(cd "$(dirname "$0")/.." && pwd)"
recordings="$repository/crates/metsuke-wire/tests/fixtures/recordings"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

for shape in samples lines; do
  # Into the scratch first: writing the fixture directly would truncate it
  # before the build that has to succeed to replace it has said anything.
  cargo run --quiet --manifest-path "$repository/Cargo.toml" \
    --package metsuke-wire --example record-submission -- "$shape" \
    >"$scratch/$shape.hex"
  mv "$scratch/$shape.hex" "$recordings/submission-$shape.hex"
  echo "recorded $(wc -c <"$recordings/submission-$shape.hex") hex characters into $recordings/submission-$shape.hex"
done
