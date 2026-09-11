# Changelog

Three things are released from this repository and they are versioned apart, so
each has its own section here. `docs/releasing.md` is the procedure, ADR 0006 is
why. `metsuke-wire` is not released and does not appear.

## metsuke, the agent

### Unreleased

- Both spellings of the Leios key's envelope `type` load. prototype-2026w36
  corrected `Mininimal` to `Minimal` in what `cardano-cli node key-gen-BLS`
  writes, so a key generated under it is refused at startup by 0.2.0 while one
  generated earlier is not. The seed behind the two is the same, so both are
  accepted rather than either rewritten.

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
  keep. Unlike `namespaces`, an exclude is not held to `namespace_roots`: one
  naming something outside them selects nothing either way, so it is accepted
  and does nothing rather than refused.
- The pipe source runs the agent inside your node's unit, so the drop-in that
  sets it up replaces four of that unit's supervision directives. `RestartSec=`
  and `StartLimitIntervalSec=`/`StartLimitBurst=` in `[Unit]` are
  `contrib/cardano-node.service`'s own values, so a node on that unit is paced
  as it already was. `Restart=` is the one that widens, to `always`: a
  pipeline's status is its last command's, and the agent exits 0 when the
  node's output ends, so `on-failure` would never fire for a node that died.
- Nothing restarts the agent if it dies while the node lives, under that source
  alone. The unit's process is the shell, which goes on waiting, so there is no
  exit for `Restart=` to act on: the agent's startup line in the node's journal
  is what says it came back.
- A pool may report under its Leios key rather than its cold key, so a
  reporting machine need hold no cold key (ADR 0011).
- The `-wal` and `-shm` beside the spool are held to 0600 whether the agent
  created them or found them. sqlite copies the database's mode onto the ones
  it creates, so a pair left behind by an unclean exit under a permissive
  umask stayed readable while holding the same signed rows.
- A `spool_path` whose directory is there but unusable says so, where it used
  to report the directory as absent and name the error that says it exists.
  A previous run under a unit of its own leaves that directory a symlink into
  `/var/lib/private`, which only root can follow, so this is what a later run
  from a shell on the same host meets.

## metsuke-server

### Unreleased

- The roster reads the BLS key where `pool-state` answers it as of
  prototype-2026w36, which renamed the field and left the registered and
  announced halves as different ledger types carrying it under different names.
  One path for both read a pool that had registered a key as registering none.
- A pool the chain registers no BLS key for is left out of the roster instead
  of failing the run. It cannot sign with a key it does not have, so refusing
  the roster only denied every pool that can. No pool listing one at all is
  still an error, that being the field having moved rather than the network,
  and `generate` reports how many pools it left out.

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
  agent already held its `upload_url` to, and it has to be a bare origin: a
  path, query, fragment or userinfo in it is refused at startup, and
  `nixosModules.metsuke-server` refuses it at evaluation rather than leaving a
  failed unit on the host. Every install command the pages print is built from
  it, and the agent's endpoint and the fetch tool's are appended to it
  differently, so a path sent the two to different roots.
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
still `0.x`. Two on-disk formats moved, which is what the number is for: the
state file and the `--into` directory. Objects keep their bytes and every
duckdb read over them still works.

- A download directory records the bar it was filled under, in
  `.metsuke-verification.json`, and a run asking for another is refused before
  it downloads anything. Two state files at two bars into one `--into` used to
  leave proven and assumed objects side by side with nothing to tell them
  apart. A directory that predates the record is claimed rather than refused:
  there is nothing to read the bar of what is already there off, so the first
  run under this version is what it is held to from then on.
- The size bound a run held objects to is recorded in the state file, and a
  file that does not carry one cannot be resumed under any bound. Reading it as
  the shipped default would have taken a low-bound run's cursor as its own and
  walked past every object that run refused.
- `--from` and `--to` refuse an instant an offset carries outside the years a
  key can name, rather than ending the process on it, and a last bound at the
  end of representable time walks to the end.
- A key whose bytes did not verify is remembered in the state file and named by
  every later run, which exits nonzero while any are held.
  `--forget-unverified` clears them once they have been dealt with, and `list`
  refuses it rather than accepting it and doing nothing: a listing keeps no
  verification state to forget.
