# Metsuke

Telemetry for the MusashiNet rewards program. A stake pool operator runs an
agent beside cardano-node. The agent reads the node's Prometheus endpoint over
loopback and sends a signed submission. A server verifies the signature, checks
the pool against an allowlist, and writes the raw signed bytes to S3. Nothing
else is stored, and no state lives on the server.

**Running a pool?** You want the quickstart the ingest server renders at its root
URL, which is four steps and the files to copy. This file is for people working
on metsuke.

Which pool a submission is from follows from the key that signed it. A cold key
hashes to the pool id, so the server derives it rather than believing a field. A
Leios key hashes to nothing, so a submission it signed claims its pool in a
header and the server believes the claim only where a roster file lists that key
for that pool. docs/adr/0011 is the decision and what each key costs.

## The four crates

| Crate | What it is | Who runs it |
|---|---|---|
| `metsuke` | The agent. Scrapes the node, spools to SQLite, sends signed submissions. | Operators, on their node host |
| `metsuke-server` | Ingest. Verifies signatures, applies the allowlist, PUTs to S3. | Us |
| `metsuke-fetch` | Delta-sync of the archive to local files. | Developers reading submissions back |
| `metsuke-wire` | The wire contract the other three agree on. | Nobody directly |

`metsuke-wire` is the only crate any of the others link. The one exception is
`metsuke-fetch`, whose tests pull from a real server, so it dev-depends on
`metsuke-server`.

## What a submission looks like

One submission is a plain JSON header frame, then the scrapes zstd compressed,
one JSON object per line, with a detached Ed25519 signature over the whole byte
sequence. The header rides in a zstd skippable frame, so `zstd -d` on a stored
object hands back the lines and nothing else. There is no COSE and no CBOR on
the submission path (ADR 0001).

A scrape carries every metric the endpoint returned, plus the time the agent
scraped and the clock offset its own SNTP query measured. A scrape that failed
is still a scrape: no metrics, and a reason naming what stopped it.

The signature and the verification key travel in two HTTP headers beside the
body. The pool id does not travel at all.

## Running it

**Operators** do not read this file. Point them at the server's root URL. It
renders a four step quickstart, with `/details` behind it holding everything the
four leave out: what a submission carries, which key signs it, and what the node
has to be told. Both are filled from the shipped configs, the shipped units, a
recording of the agent's own output and the wire types themselves, so neither
can document a default the agent does not ship. The markup is
`crates/metsuke-server/assets/`, and `instructions.rs` fills it.

**Deploying a server and an agent** is `docs/deploying.md`.

**Reading the archive back** is `docs/reading-the-archive.md`.

## Working on it

Everything happens inside the flake devShell, which direnv picks up.

`just` lists the recipes, with what each one runs and how long it takes.
`just all` is what a commit should pass. Every recipe writes its whole output
under `.test-logs/` and prints the path, so grep that when the summary is not
enough.

### Flake outputs

`nix flake show` enumerates them. Two are worth naming here: `nixosModules`
holds `metsuke` for the agent and `metsuke-server`, and those are what a
deployment imports. There is no overlay.

### Repository layout

```
crates/           the four crates
nix/              NixOS modules, the shared unit, and the VM tests
contrib/          example agent config, example server config, example unit
tools/allowlist/  the offline application-code gate, in nushell
scripts/          fixture recorders and the test reporter
devnet/           a local Leios devnet the fixture recorders drive
docs/adr/         accepted decisions
docs/research/    notes behind them
```

## Documentation

`CONTEXT.md` is the domain language. Read it first, and treat it as binding.
Terms on an entry's _Avoid_ list do not belong in code or prose.

`docs/adr/` holds accepted decisions, not proposals. Their filenames say what
each decided; read the one covering ground you are about to work near before you
work near it. `CLAUDE.md` lists the ones that constrain everyday code, and
`docs/adr/README.md` says what an ADR is allowed to change after it is accepted.

## Versioning

Client and server are versioned and tagged independently, and the update nudge
is embedded at server build time. `docs/releasing.md` is the procedure, ADR 0006
is why.

## Security

Least privilege is the top constraint, and the agent is where it is spent. It
never opens the node socket, and it reads exactly one key, the cold or Leios
signing key the operator points it at. A cold key has to hash to the configured
pool id or the agent refuses to start; a Leios key hashes to nothing, and
keeping the cold key off a reporting machine is what it is for (ADR 0011).

What the `[log]` section costs in privilege, and why that made the feature
opt-in, is ADR 0010. Its two sources do not cost the same: `journald` needs the
`systemd-journal` group, which reads every unit's journal, and `pipe` reads the
node's stdout on the agent's stdin and needs no group at all. `CLAUDE.md`
states the invariant.

Report a vulnerability to the maintainers privately rather than opening a public
issue.

## License

Apache-2.0. See `LICENSE` and `NOTICE`.
