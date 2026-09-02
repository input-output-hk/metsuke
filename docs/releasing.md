# Releasing

The procedure behind ADR 0006. Read the ADR for why client and server are
versioned apart. This file is what you actually run.

## What carries a version

Three of the four crates are released. The test is whether the crate leaves
something behind that outlives one run and is read by someone who did not build
it.

`metsuke` is the agent, tagged `client-vX.Y.Z`. Its version is what an operator
sees, what the agent logs at startup, and what the server's ACK compares
against. Tagging one is a statement that operators should take it.

`metsuke-server` is tagged `server-vX.Y.Z`. Nobody outside the team reads this
version. It exists so a deploy can be named.

`metsuke-fetch` is tagged `fetch-vX.Y.Z`. It writes two things that outlive the
run that wrote them: the cursor file `--state` names, which it reads back and
validates on the next run, and the tree under `--into`, which duckdb then reads
straight off disk. Neither is ours to break quietly, and a Developer reading the
archive is not required to be someone who can build the tool. Its compatibility
surface is those two formats, not its Rust API.

`metsuke-wire` is not released and is not tagged. Nobody runs it, and the
contract it actually holds already has a version that is not a crate version,
`schema_version` on the envelope, whose values `envelope.rs` declares. Its
manifest version means nothing. Do not bump it and do not read it.

All four crates set `publish = false`. Nothing goes to crates.io.

## Which number to bump

Semver, read against the operator rather than against the API.

**Patch** is a fix that changes nothing an operator configured or reads. Bump it
freely.

**Minor** is a new config field with a default, a new metric the agent collects,
or a change to what the onboarding page tells an operator to do. An operator can
update without touching their config, but may want to.

**Major** is a config field that changed meaning or went away, a new required
field, or a `schema_version` bump. An operator has to do something. We have not
shipped one and should not want to.

The rule that decides the case: if updating the agent without also editing
`/etc/metsuke/config.toml` leaves a working agent, it is not major.

For `metsuke-fetch`, read the same three against its two on-disk formats. A
cursor file the new build cannot read, or a change to the tree layout under
`--into` that breaks a duckdb read someone already wrote, is major.

## Releasing the agent

The nudge only reaches operators through a server that was built after the bump,
so the order matters.

1. Bump `version` in `crates/metsuke/Cargo.toml`.
2. Add the release to `CHANGELOG.md` under the new version, with today's date.
3. `just all`. It has to be green, including the VM tests. An agent release is
   the one thing here we cannot roll back for people.
4. Commit, then tag `client-vX.Y.Z`.
5. Redeploy the server. `metsuke-server`'s `build.rs` reads the agent's manifest
   at compile time, so until the server is rebuilt it keeps telling every agent
   the old version is current. This step is the release, and the tag is only a
   name for it.
6. Confirm the server is serving the new number. It is in the quickstart's at a
   glance table, in its update step, and in every ACK:

   ```
   curl -s https://<server>/ | grep 'Agent version'
   ```

7. Tell operators. There is no self-update and no install script, by decision,
   so an update happens only because someone chose to do it.

## Releasing the server

1. Bump `version` in `crates/metsuke-server/Cargo.toml`.
2. Add the release to `CHANGELOG.md`.
3. `just all`.
4. Commit, then tag `server-vX.Y.Z`.
5. Deploy.

A server-only release nudges nobody, which is the point of ADR 0006. It still
rebuilds against whatever agent version the manifest currently holds, so a
server deploy after an unreleased agent bump would ship that number early. Do
not bump the agent's manifest until you mean to release it.

## Releasing the fetch tool

1. Bump `version` in `crates/metsuke-fetch/Cargo.toml`.
2. Add the release to `CHANGELOG.md`.
3. `just all`.
4. Commit, then tag `fetch-vX.Y.Z`.

There is nothing to deploy. Nobody is nudged, and nothing tells a Developer
their copy is behind, so a release that changes either on-disk format is one to
announce rather than assume.

## Checking the nudge works

`metsuke::uploader::newer_version_available` compares dot-separated segments
numerically, and a version that does not parse as digits cannot claim to be
newer. That means a tag suffix like `1.2.0-rc1` in a manifest silently disables
the nudge for everyone running it. Keep manifest versions to digits and dots.

The agent logs the warning to its journal, once per submission whose ACK names a
newer version. An operator who never reads their journal never sees it, which is
accepted.
