# Scrape fixtures

Cassette bodies for the PrometheusSimple scrape tests in tests/scrape.rs,
replayed via wiremock.

- `recordings/` — real bodies captured from a Leios node by
  scripts/record-scrape-fixtures.sh. Never edit by hand. Re-record on every
  bump of the `cardano-node-leios` input in flake.nix, and commit the new
  bodies together with the updated expected values in tests/scrape.rs in the
  same change.
- `edge-cases/` — hand-authored bodies for deliberate parser edge cases. No
  provenance: every value in them is invented, which is why they live apart
  from the recordings.
