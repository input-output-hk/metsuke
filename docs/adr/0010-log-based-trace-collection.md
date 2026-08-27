# 10. Trace lines off the journal, selected by configuration

Status: accepted (2026-08-27). Supersedes ADR 0007.
Amended by metsuke-jfb.11, which moved where a line's stamp is applied, by
metsuke-jfb.19, which dropped severity as a selection rule, and by
metsuke-4zo.98, which settled what the archive says about lost lines: nothing,
and by metsuke-4zo.107, which made a prefix match on segment boundaries.

## Context

ADR 0007 refused the journal because the agent "may eventually run on mainnet
block producers". That premise no longer holds: the project owner has stated
there will be no mainnet in the foreseeable future. What survives from ADR 0007
is that the privilege set is the attack surface — the reason changed, not the
caution.

The rewards-program developers asked for the distributions of announcement
receipt, of EB body and closure receipt, of quorum, of `LeiosNotVoted` and of RB
adoption, plus every trace at error, warning or notice severity — the one ask
metsuke-jfb.19 drops. Every distribution maps to a namespace the node emits, all
under `Consensus.LeiosKernel` and `Consensus.LeiosPeer` except RB adoption,
which is neither:
`ChainDB.AddBlockEvent.AddedToCurrentChain` and `Forge.Loop.AdoptedBlock`. They
compute the distributions themselves, and said their list is incomplete.
None of it is answerable from the Prometheus endpoint: one scrape is a periodic
snapshot of chain state with no per-event timestamps, so "when did this arrive"
has no field to read. The data exists only in the node's trace stream.

Two transports carry that stream. `journalctl --follow` on the node's unit is
out of band: an agent that stalls or dies leaves the node untouched. Reading
the node's stdout as a pipe needs no privilege at all, but puts metsuke inside
the block producer's write path, where a stalled reader is the node's problem.

## Decision

The agent reads the node's trace stream from the source `[log].source` names,
selects lines by namespace prefix — configuration — and ships every field of
what it selects. The journal source follows `journalctl --follow`; the pipe
source reads the node's own stdout and tees it through untouched.
It parses the line as a JSON object and reads `ns` off its top level and nothing
else; a line the parse refuses declares no namespace, so no rule reaches it.

A prefix matches on segment boundaries, at both seams: a rule against the
`namespace_roots` ceiling and a line's namespace against a rule. A rule that
stops mid-segment selects nothing rather than whatever shares its letters, so an
entry names a namespace or an ancestor of one and never a fragment. The roots
are spelled without a trailing dot, because the boundary is the rule's rather
than the spelling's (metsuke-4zo.107).

Severity is not a rule. A namespace's severity is assigned by the node's own
`TraceOptions`, so what a line carries in `sev` states what its operator
configured rather than whether anyone asked for the line, and a floor over that
selects a set that changes with the node's config and with a node version's
spelling of the ladder. So the severity ask is dropped, not answered another
way. What that costs is bounded: every distribution the program asked for is
already a namespace in the shipped `namespaces` default, so the floor added no
measurement line. What it added was error, warning and notice lines from
namespaces nobody named, and no stated use exists for those.

That parse is also what travels: a selected line is the object from here on, so
the spool and the wire hold it re-rendered rather than as the node's own bytes,
and nothing downstream parses it a second time. It does not know what a Leios
trace means and computes nothing from one. One more namespace is therefore an
edit to a config file rather than a release, bounded by a `namespace_roots`
ceiling the host sets: the agent can read every unit's journal, and a namespace
rule is what reaches into it by name.

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

Both sources are built, behind one `LineSource`, and `[log].source` picks
between them with no default: an agent that guessed would collect nothing or
tee nothing and say neither. The pipe holds no group at all, so a host that
runs `cardano-node run | metsuke` pays none of what this ADR prices. The NixOS
module renders only the journal, because the unit it writes has no node
upstream of it.

## Consequences

- Trace collection has a node-config step beyond ADR 0007's. Adding the stdout
  backend is not enough: the node has to be told to emit the named namespaces at
  all. The agent cannot select a line the node never wrote, which is the one way
  severity still bounds what reaches the archive. The step names every namespace
  the node must emit for the agent's rules to select anything, each with its own
  severity, so it holds under whatever root threshold the operator has and the
  page states no floor they must stay under. Both node-config snippets are keys
  to merge, never a `TraceOptions` to paste over one: an operator's own config
  may already carry these namespaces and settings on them that replacing the
  object would discard. The instructions page carries the step and
  docs/research/cardano-node-11-tracing.md carries why, and what the published
  configs hold.
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
- Nothing in the archive says a line is missing, and that is settled rather than
  pending: metsuke-4zo.98 weighed a loss record in the payload and refused it.
  Of the five paths that can lose lines, four do not occur at the shipped caps —
  the retention figure is on metsuke-uxw's comments — and the fifth is a restart,
  which is a window with no count in it, so a record would state a guess. Adding
  one would put a rare shape in the stream every consumer reads, which is what a
  columnar reader handles worst (metsuke-4zo.112). Loss reaches the operator
  through the agent's counters (metsuke-4zo.109) and the program through absence
  in the bucket (metsuke-4zo.110); a large gap stays visible as a hole in the
  timestamps of the lines around it, and a small one is not worth a shape.
- The restart gap is worth closing rather than describing. `--show-cursor` and
  `--after-cursor` resume exactly with `--output=cat` intact, if the follow model
  becomes a polled read (metsuke-4zo.114). It cannot help `source = "pipe"`: a
  pipe has no cursor and nothing to replay.
- A misconfigured `journal_unit` is still silent while the agent runs:
  `journalctl --follow` on a unit that does not exist waits forever rather than
  failing, and the agent has no "no lines in N minutes" signal. A journalctl that
  cannot be executed does fail the start; one refused the journal read exits
  after the exec succeeded, so it is still a warning per backoff
  (metsuke-4zo.116). A stream that ends now carries the status its journalctl
  exited with, which is what separates those two ends.
- The end-to-end test carries a real node's selected lines all the way into the
  bucket, but not a Leios round's: one node forges an EB only if its own
  mempool overflows, and never receives an announcement or reaches a quorum,
  because both need a peer. What that test reads back is a node start and the
  blocks the node forges alone, which between them cover every namespace the
  agent ships selecting. What a Leios round's traces look like is owned by the
  recordings under crates/metsuke/tests/fixtures, replayed through a real
  journal in the unit test and through the selection and spool in `cargo test`.
- Selecting costs a JSON parse of every line the node writes, wanted or not,
  and now the parsed object as well. That is deliberate: reading `ns` as a
  substring needed a rule about which occurrence in the line is the record's
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
