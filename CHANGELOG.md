# Changelog

Three things are released from this repository and they are versioned apart, so
each has its own section here. `docs/releasing.md` is the procedure, ADR 0006 is
why. `metsuke-wire` is not released and does not appear.

## metsuke, the agent

### Unreleased

Nothing since 0.2.0.

### 0.2.0 — 2026-09-07

A **major** by `docs/releasing.md`, in the minor position because the number is
still `0.x`. The first version a server hands out: that file calls the server
deploy the release and the tag only a name for it, so this entry precedes a
`client-v0.2.0` tag rather than waiting on one.

- `scrape_interval_secs` and `upload_interval_secs` have to be above zero. A
  config setting either to `0` stops the agent at startup, and the NixOS module
  refuses to evaluate. Zero meant a busy loop, never a disabled one, so nothing
  that worked stops working.
- Trace lines, not only metrics. `[log]` turns them on and `[log].source`
  picks where they are read from, `journald` or `pipe`; ADR 0010 prices what
  each costs and neither is the default. Without the section the agent reads
  the loopback metrics endpoint alone, as before.
- `[log].exclude_namespaces` drops namespaces the selection would otherwise
  keep, under the `namespace_roots` ceiling.
- A pool may report under its Leios key rather than its cold key, so a
  reporting machine need hold no cold key (ADR 0011).

## metsuke-server

### Unreleased

Nothing since 0.2.0.

### 0.2.0 — 2026-09-07

A **major** by `docs/releasing.md`, in the minor position because the number is
still `0.x`. Three changes an operator on an earlier build would have had to
act on. Nothing had shipped, so nobody had to.

- `[downloads]` is a map from the name a build is served under to its path,
  where it was two fixed fields. A deployment still on the old keys serves its
  builds under names no page links, and the install step falls back to telling
  operators to build one instead — which is a silent failure, not a refusal.
  It buys serving any build, which is how `metsuke-fetch` is offered to a
  developer without putting it in front of a pool operator.
- The developer accounts file is one `user = "password"` line per account
  rather than a single shared password, so several people can pull the archive
  under names of their own and one can be revoked by editing one line. A file
  in the old format stops startup.
- `public_url` has to be https, or http on a loopback host, which is what the
  agent already held its `upload_url` to. Every install command the pages print
  is built from it.
- A `[downloads]` entry naming a file the server already serves stops startup,
  rather than serving one file and publishing the other's checksum beside it.
- A roster naming one pool twice is refused instead of taking whichever line
  came last.
- The pages answer `HEAD`, and every answer carries a content security policy,
  `nosniff` and `no-referrer`.

## metsuke-fetch

### Unreleased

Nothing since 0.2.0.

### 0.2.0 — 2026-09-07

A **major** by `docs/releasing.md`, in the minor position because the number is
still `0.x`. A state file written before this version is refused, which is the
change the number is for. Sync into a new one; the objects already on disk are
unaffected.

- The size bound a run held objects to is recorded in the state file, and a
  file that does not carry one cannot be resumed under any bound. Reading it as
  the shipped default would have taken a low-bound run's cursor as its own and
  walked past every object that run refused.
- `--from` and `--to` refuse an instant an offset carries outside the years a
  key can name, rather than ending the process on it, and a last bound at the
  end of representable time walks to the end.
- A key whose bytes did not verify is remembered in the state file and named by
  every later run, which exits nonzero while any are held.
  `--forget-unverified` clears them once they have been dealt with.
