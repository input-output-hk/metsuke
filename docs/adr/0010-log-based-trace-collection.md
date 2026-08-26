# 10. Trace lines off the journal, selected by configuration

Status: proposed (2026-08-25), supersedes ADR 0007, where a line's stamp is
applied amended by metsuke-jfb.11

## Context

ADR 0007 refused the journal because the agent "may eventually run on mainnet
block producers". That premise no longer holds: the project owner has stated
there will be no mainnet in the foreseeable future. What survives from ADR 0007
is that the privilege set is the attack surface — the reason changed, not the
caution.

The rewards-program developers asked for the distributions of announcement
receipt, of EB body and closure receipt, of quorum, and of RB adoption, plus
every trace at error, warning or notice severity and every `LeiosNotVoted`.
They compute the distributions themselves, and said their list is incomplete.
None of it is answerable from the Prometheus endpoint: one scrape is a periodic
snapshot of chain state with no per-event timestamps, so "when did this arrive"
has no field to read. The data exists only in the node's trace stream.

Two transports carry that stream. `journalctl --follow` on the node's unit is
out of band: an agent that stalls or dies leaves the node untouched. Reading
the node's stdout as a pipe needs no privilege at all, but puts metsuke inside
the block producer's write path, where a stalled reader is the node's problem.

## Decision

The agent follows the node's journal with `journalctl --follow`, selects lines
by namespace prefix or severity floor — both configuration, matched as *or*,
since "every Leios event" and "every error, warning and notice" are two
independent asks — and ships every field of what it selects. It parses the line
as a JSON object and reads `ns` and `sev` off its top level and nothing else; a
line the parse refuses declares neither field, so no rule reaches it. That parse
is also what travels: a selected line is the object from here on, so the spool
and the wire hold it re-rendered rather than as the node's own bytes, and
nothing downstream parses it a second time. It does
not know what a Leios trace means and computes nothing from one. One more
namespace is therefore an edit to a config file rather than a release, bounded
by a `namespace_roots` ceiling the host sets: the agent can read every unit's
journal, and a namespace rule is what reaches into it by name.

Collection is opt-in. Without a `[log]` section the agent reads only the
loopback metrics endpoint and holds no group, which is ADR 0007's posture
unchanged. With one, the unit gains `SupplementaryGroups=systemd-journal` and
`ProcSubset=all`, and nothing else: no capability, no node socket, no write
beyond the state directory. The two are one parameter in nix/unit.nix, because
a unit holding the group without the second cannot start journalctl at all.

Selected lines land in the existing spool under their own byte cap and upload
as schema v2 envelopes on the same signed path as samples (ADR 0001, 0005).
They are the data frame's lines, each the node's object with the agent's
provenance added under the one reserved `metsuke` key, so a trace line and a
metrics line have the same shape and one query over the archive reads both. One
reserved key rather than the provenance fields merged into the line's top level:
what a node writes there is the node version's to name, so a merge would need a
rule about which names metsuke may take, and one reserved key is a constraint
statable in a sentence. One envelope carries one kind of payload, and only the
upload loop seals, so the two streams share an agent's counter without
coordinating.

The stamp goes on before the row reaches the spool, so a stored row is the line
it will be on the wire and taking a batch concatenates rows rather than reading
any of them back (metsuke-jfb.11). Two things follow. A row costs exactly what
the spool recorded for it, which is the one number the agent's batch budget and
the server's decompress limit both count
(`envelope::PayloadLine::wire_bytes`). And no later change to a payload schema
can refuse a line already written: what the spool holds is text this build
produced, not a value some future build has to agree with.

Pipe mode is not built. It is a second implementation behind the same line
source, and its own decision.

## Consequences

- Trace collection has a node-config step beyond ADR 0007's. Adding the stdout
  backend is not enough: every Leios namespace is emitted below the node's own
  root severity threshold, and that threshold is also the ceiling on what the
  agent's severity floor can ever see. The instructions page carries the step
  and docs/research/cardano-node-11-tracing.md carries why.
- The grant is real and lasts as long as `[log]` is set. `systemd-journal`
  reads the whole system journal, not the node's unit: an agent compromised on
  a host that logs anything sensitive to the journal reads that too. The
  `--unit` filter is journalctl's, not the kernel's. `ProcSubset=all` is the
  smaller half of the same trade: the process sees the non-task parts of
  `/proc` again, which `ProtectKernelTunables` still keeps read-only.
- The agent spawns a child process it did not before. systemd's control-group
  default kills it with the agent, but it runs under the agent's identity at a
  path whoever can write the config file chooses — the reach writing
  `upload_url` already gives, through a different mechanism.
- A respawned journalctl resumes at the journal's current end, so every gap
  between an unexpected exit and a successful respawn is lost silently. No
  cursor is persisted. Whether that is acceptable for the distributions the
  developers are computing is not settled here.
- A misconfigured `journal_unit` is silent: `journalctl --follow` on a unit
  that does not exist waits forever rather than failing, and the agent has no
  "no lines in N minutes" signal.
- The end-to-end test carries a real node's selected lines all the way into the
  bucket, but not a Leios round's: one node forges an EB only if its own
  mempool overflows, and never receives an announcement or reaches a quorum,
  because both need a peer. A node start fires both rules and is what that test
  reads back. What a Leios round's traces look like is owned by the recordings
  under crates/metsuke/tests/fixtures, replayed through a real journal in the
  unit test and through the selection and spool in `cargo test`.
- Selecting costs a JSON parse of every line the node writes, wanted or not,
  and now the parsed object as well. That is deliberate: reading the fields as
  substrings needed a rule about which occurrence in the line is the record's
  own, and that rule is a claim about the node's key order that nothing
  enforces. Nobody has measured what the parse costs on a real block producer; a
  prefilter ahead of it is a local change if it turns out to matter.
- A shipped line is the node's fields, not the node's bytes: keys arrive sorted,
  escapes are rewritten, and a number is re-rendered as the parser read it.
  Nothing compares the re-rendering against the node's own text, so what the
  archive holds is what this build's JSON parser and writer agree the line
  meant. The refusals are the bound: `TraceLine::parse` takes only one whole
  object, and a line naming `metsuke` is not one.
- An upload tick now costs two envelopes where it cost one, and the trace
  stream's volume is not the sampler's. The shipped caps are starting numbers,
  not measured ones: the volume behind them was a flood-driven burst on a
  devnet, not an SPO running the configuration the instructions page asks for.
