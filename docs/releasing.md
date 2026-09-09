# Releasing

The procedure behind [ADR 0006](adr/0006-independent-crate-versions.md). Read
the ADR for why client and server are versioned apart. This file is what you
actually run.

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

**Before 1.0.0, read those three one place to the right.** A major by the rules
above goes in the minor position and a minor or a patch goes in the patch one,
because the leading zero is what says there is no contract to break yet. A
release that would be 2.0.0 after the cut is 0.2.0 before it. Say which of the
three a release was in its changelog entry, or the number cannot be read back:
0.2.0 and 0.2.0 across two crates may be one break and one addition.

**1.0.0 is the exception to the paragraph above.** Going from `0.x` to `1.0.0`
breaks nothing and requires nothing of an operator. It says the config fields
and the module options are now a contract, which is what the three rules above
already assume and what a `0.x` number tells an operator not to assume. Cut it
when a release goes by without an operator-facing rename, not on a date. After
it, a major means what this file says it means.

For `metsuke-fetch`, read the same three against its two on-disk formats. A
cursor file the new build cannot read, or a change to the tree layout under
`--into` that breaks a duckdb read someone already wrote, is major.

## What a tag publishes

`client-v*` and `fetch-v*` each attach that crate's two static builds to a
draft release, with a `.sha256` beside each one. The asset names are the
flake's own output names, which is what a deployment serves the same builds
under, so `sha256sum -c` reads the name back out of the sidecar either way.
`server-v*` publishes nothing, since the deploy is the release.

The draft is left for you to write a description on and publish. Nothing in CI
writes one, and a tag lands before the server is redeployed, so a release
published at tag time would name a version the onboarding page does not yet
serve.

Nothing is compiled to make those assets. The job waits up to thirty minutes
for the tag's builds and for `hydraJobs.required-x86_64-linux` to reach
`cache.iog.io`, then substitutes them, so an asset is the bytes Hydra built or
the release does not happen. The aggregate is the part that makes step 4 more
than a promise: it is built only where every check and the VM units for that
arch were, so a tag on a commit whose suite fails cannot publish.

Tagging ahead of Hydra is fine, and tagging something Hydra cannot build is
what fails. A queue slower than thirty minutes fails the same way, so re-run
the workflow rather than reading it as a verdict. A re-run reaches the same
end state while the release is still a draft.

The onboarding page still documents the download from the deployment an
operator is already talking to. A deployment pins each of those builds at its
own release tag, so what it serves and what the release carries are the same
bytes, and they stay so across server deploys that have nothing to do with the
agent. `docs/deploying.md` has the pins.

GitHub shows one release as Latest, which is what the repository's landing page
and `/releases/latest` resolve to. Publishing a fetch draft would take it from
the agent's, so leave "Set as the latest release" unchecked on that one.

## Releasing the agent

The nudge only reaches operators through a server that was built after the bump,
so the order matters.

1. Bump `version` in `crates/metsuke/Cargo.toml`.
2. `METSUKE_RERECORD=1 cargo test -p metsuke --test binary`. The page's check
   step shows a recorded agent journal, and its first line carries the version,
   so without this the page shows the release before this one.
   `the_recorded_journal_names_this_version` fails until you do it.
3. Add the release to `CHANGELOG.md` under the new version, with today's date.
4. `just all`. It has to be green, including the VM tests. An agent release is
   the one thing here we cannot roll back for people.
5. Commit, then tag `client-vX.Y.Z` and push the tag, which drafts the release
   and attaches the two static builds to it.
6. Move the deployment's client pin to this tag and redeploy the server.
   `metsuke-server`'s `build.rs` reads the agent's manifest at compile time, so
   until the server is rebuilt it keeps telling every agent the old version is
   current. This step is the release, and the tag is only a name for it.
   `docs/deploying.md` has the pin and why the downloads come from it rather
   than from the input the server is built from.
7. Confirm the server is serving the new number. It is in the quickstart's
   staying up to date step, and in every ACK:

   ```
   curl -s https://<server>/ | grep 'built against agent'
   ```

8. Write the draft release's description and publish it. Its assets are not
   reachable without a token until you do, which is what the next step needs.
9. Confirm the release asset and the served copy are the same bytes:

   ```
   curl -fsS https://<server>/files/metsuke-static-x86_64-linux.sha256
   curl -fsSL https://github.com/input-output-hk/metsuke/releases/download/client-vX.Y.Z/metsuke-static-x86_64-linux.sha256
   ```

   They are identical when the deployment's release pin is this tag, which is
   what this is checking. A difference means the pin did not move with step 6,
   so operators are being handed a build this release did not publish.

10. Tell operators. There is no self-update and no install script, by decision,
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
4. Commit, then tag `fetch-vX.Y.Z` and push the tag, which drafts the release
   and attaches the two static builds to it. Publish the draft with "Set as
   the latest release" unchecked, so the agent's release keeps it.
5. Move the deployment's fetch pin to this tag and redeploy.
   `docs/deploying.md` has the pin.

Unlike the agent's, the deploy is not the release here: the tag and its assets
are what a Developer needs, and step 5 only keeps the copy under `/files/`
current. Nobody is nudged, and nothing tells a Developer their copy is behind,
so a release that changes either on-disk format is one to announce rather than
assume.

## Checking the nudge works

`metsuke::uploader::newer_version_available` compares dot-separated segments
numerically, and a version that does not parse as digits cannot claim to be
newer. That means a tag suffix like `1.2.0-rc1` in a manifest silently disables
the nudge for everyone running it. Keep manifest versions to digits and dots.

The agent logs the warning to its journal, once per submission whose ACK names a
newer version. An operator who never reads their journal never sees it, which is
accepted.
