# 6. Independent client/server versions; update nudge embedded at build

Status: accepted (2026-08-19)

## Context

The client runs on operator machines we do not control; the server we redeploy
freely. A workspace-wide version would make every server bugfix look like a client
release and nudge operators to update for nothing. There is no self-update and no
install script, ever. Updating is a deliberate operator act.

## Decision

Every crate somebody runs is versioned and tagged on its own, `<crate>-vX.Y.Z`,
rather than the workspace carrying one number. A crate nobody runs is not
tagged.

The server's upload ACK carries `latest_version`, the client crate's version
read from the workspace at server compile time. The client logs a journald
warning when it is older. Release cadence is the filter. A client version is
tagged only when operators should take it.

## Consequences

- A server-only fix bumps only the server and nudges nobody; shipping a client
  release requires a server redeploy to update the embedded version.
- No version registry, no config knob, no network lookup. The nudge cannot claim a
  version that wasn't built.
- If client releases ever outpace server deploys, a config override is the
  designated escape hatch. Add it then, not now.
