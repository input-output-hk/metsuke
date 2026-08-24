# 9. The server reads db-sync over the Postgres wire protocol

Status: accepted (2026-08-24)

## Context

Both chain reads — the Calidus registrations and the allowlist gate's
registered codes — ran a query through a `psql` subprocess and parsed its CSV.
The interface carried the query text on stdin and its values as `--set`
variables, because `--command` does not interpolate `:'var'`; a regression to
`--command` was a total outage the suite could not see, since the double it
tested against was a hand-written shell script rather than a recording. Three
review rounds of metsuke-4zo.44 produced roughly twelve findings, and six of
them were guards on one spelling of that one argv.

ADR 0008 recorded a single consequence against the alternative: a Postgres
client brings an async runtime into a server that had none. It did not argue
the point, and nothing else in the repo did either.

## Decision

Both callers use the `postgres` crate. Parameters are bound in the protocol, so
the query text is fixed at compile time and no value can reach the parser. The
registrations query returns `bytea` rather than `encode(tm.bytes, 'hex')`.

A connection is opened per query rather than pooled: `DbSync::registrations`
runs behind the resolution TTL and the gate is a one-shot command, so the round
trip is paid rarely and buys a `&self` caller with no lock and no reconnect
state.

## Consequences

- The runtime ADR 0008 named is real and now present: `postgres` drives a
  current-thread tokio internally. It is not shared with the HTTP layer, which
  stays `tiny_http` and `ureq`; whether that layer goes async is metsuke-a3a's
  to decide, on its own grounds. Static builds are unaffected — the crate is
  pure Rust with no libpq, and metsuke-4zo.12 scopes those outputs to the agent
  anyway.
- The parser moves in-process, and the process boundary goes with it. This is
  accepted residual risk, not a wash. Under `psql`, a parser bug landed in a
  short-lived child holding no S3 credentials, no submission index and no
  server state; now it lands in the server. What it takes to reach either is the
  same — a compromised db-sync, which is loopback, read-only and colocated —
  but the consequence is strictly worse, and no boundary replaces the one that
  was removed. Accepted because the surface traded away produced an outage and
  the surface taken on has produced none, which is a bet on incident history
  rather than on a security argument.
- `psql_path` leaves both config sections, and `password_file` now holds the
  password alone rather than a `.pgpass`, because the client reads the file
  itself. Neither password enters the environment.
- No test double answers the wire protocol. `fake-psql.sh` and every argv
  assertion are gone, and with them the end-to-end proof that
  `generate-allowlist` writes pairs to stdout — `gate` and `Gate::to_toml`
  carry that at unit level, and the binary test now proves only that an
  unreachable db-sync exits nonzero with stdout untouched. Restoring
  end-to-end coverage takes a real Postgres in `nix flake check`, which
  metsuke-4zo.12 would have to keep hermetic.
