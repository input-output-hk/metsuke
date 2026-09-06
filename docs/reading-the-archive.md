# Reading the archive

What a consumer runs over a directory `metsuke-fetch sync --into` wrote, and
why that read is not the obvious one. Fetch first, then read: the sections are
in that order.

## The tools

Three programs read an archive, and each is a flake output pinned to what this
page measured against, so a host that has none of them needs no checkout and no
devShell:

```
nix run github:input-output-hk/metsuke#metsuke-fetch
nix run github:input-output-hk/metsuke#duckdb
nix run github:input-output-hk/metsuke#zstd
```

`metsuke-fetch` is the one that pulls the archive down, and it cross-builds to
one static file for a host with no nix at all, on either architecture:

```
nix build github:input-output-hk/metsuke#metsuke-fetch-static-x86_64-linux
nix build github:input-output-hk/metsuke#metsuke-fetch-static-aarch64-linux
```

A deployment may also serve those builds under `/files/`, in which case `curl`
the one you want from the server you pull the archive from. Its `/analysis`
page is the short version of this document against that particular deployment,
and offers them along with the two init files below. The pool operator's pages
do not: they are a different audience, which is why the analysis page is a
third one.

## Checking that an object is a pool's

The signature is detached, so it does not travel inside the object. A download
carries it beside the bytes, in the two headers the pool itself sent:

```
x-metsuke-vkey       the pool's cold verification key, hex
x-metsuke-signature  raw Ed25519 over the body as downloaded, hex
```

Two things make an object the pool's, and both are needed. The signature has to
verify over the bytes exactly as they arrived, uncompressed and unmodified, and
the key has to be the one the pool id in the object's own name is the
blake2b-224 hash of. The first says a holder of that key sealed these bytes; the
second says which pool that key speaks for. Checking one without the other
accepts an object signed by a stranger, or one filed under a pool that never
sent it.

Only a Cold Key gives you the second half. A Leios Key hashes to nothing, so an
object it signed has its signature checked like any other and its pool taken on
the server's word, against a roster that no longer exists
([ADR 0011](adr/0011-leios-key-submissions.md)). Its bytes are as proven as a
cold-signed object's; only which pool sent them is not.

An object that arrives without those headers cannot be checked at all. That is
what a filesystem archive answers, having discarded the pair at ingest rather
than merely not serving it, and what any object something other than this
server wrote answers. Bytes without them are bytes the server is asking to be
believed about.

`metsuke-fetch sync` checks every object as it lands and counts the three
outcomes apart:

```
3 keys under v1/; 3 selected, 0 outside the selection, 0 this build cannot name
3 objects into ~/archive, 3145728 bytes; 1 cold-signed, 2 Leios-signed, 0 unattested, 0 not written
```

The first line is of the keys the prefix listed, not of the archive: what the
prefix never returned is counted nowhere.

- **cold-signed** — signature checked and pool derived. Stays checkable from
  the object alone, forever.
- **Leios-signed** — signature checked, pool on the server's word.
- **unattested** — nothing checked, because nothing came with it.

An object that fails is named on stderr, is not written, and makes the run exit
nonzero. The cursor still advances past it, because the archive is append-only
and the same bytes will fail the same way tomorrow; syncing it again means
rewinding the state file. What to do about one is not this tool's:
`metsuke-server verify-archive` walks the bucket and names every object whose
stored bytes and metadata disagree, and removing one is a bucket-admin action,
since the server holds no delete.

**That exit code speaks for one run, and nothing remembers it.** The cursor is
past the refused key, so the next run over the same state file does not reach
it again, finds nothing wrong, and exits zero. Do not wrap this in a retry: a
`sync || sync`, or a timer that runs it twice, turns a run that refused objects
into a green one and the only record of them was the first run's stderr. Keep
that output, or read the exit code of the run that produced it.

All three are counted rather than refused, or a run against a filesystem
archive would download nothing. Two flags raise the bar, and each writes only
what it names:

```
--require-attested      cold-signed and Leios-signed
--require-cold-signed   cold-signed only
```

`--require-attested` is what most consumers want. Every object it writes has
had its signature checked here, over the bytes as downloaded.

`--require-cold-signed` keeps only what a later reader can recheck without
this server. Note what that costs as pools stop holding cold keys on reporting
machines, which is what ADR 0011 is for: it discards more and more of an
archive whose bytes are all provably authentic, and eventually most of it.

## Slicing by time

`--from` and `--to` bound a run by day or by instant, both inclusive:

```
metsuke-fetch sync --from 2026-08-28 --to 2026-09-02 --state range.json --into archive
metsuke-fetch sync --from 2026-09-01T08:00:00Z --to 2026-09-01T09:00:00Z --state hour.json --into archive
```

Both work for the same reason the archive sorts chronologically: a day folder
is derived from the uuidv7 that follows it, and a uuidv7's first 48 bits are
its millisecond, so a key range is a time range down to the millisecond.

**The bound is receipt time, not observation time.** The uuid is stamped when
the server accepted the submission, and the rows inside were taken before that:
by up to `upload_interval_secs` in the ordinary case, and by *days* where an
agent wedged, or drained a spool it had been filling while it could not upload.
That gap has no ceiling. A day bound is no safer here than an instant one; both
get you approximately the right files and neither gets you exactly the right
rows.

Row-level time is in the payload, and it is exact. Slice there instead, in
duckdb, which reads the downloaded objects where they landed and is what The
read below is about:

```sql
select * from read_json('archive/v1/*/*-metrics.jsonl.zst', sample_size=-1)
where scraped_at between '2026-09-01T08:00:00Z' and '2026-09-01T09:00:00Z'
```

Trace lines carry `at` for the same purpose. So widen the flags relative to the
window you want, and narrow in SQL. What the flags are for is not fetching
fewer rows, it is fetching fewer bytes.

## One state file per set of filters

A run advances the cursor past every key it saw, including the ones its filters
passed over. So a state file means nothing except against the filters it was
made with, and `sync` refuses to read one under others rather than resume past
objects it never downloaded:

```
metsuke-fetch stopped: the state file cursor.json is for another run
  holds: prefix "v1/", kind metrics, --require-cold-signed, from "v1/2026-09-01"
  asked: prefix "v1/", kind logs, no --require flag
  name a state file of its own for this one
```

They may share one `--into`, since every object lands under its own key:

```
metsuke-fetch sync --kind metrics --state metrics.json --into ~/archive
metsuke-fetch sync --kind logs    --state logs.json    --into ~/archive
```

`--to` is the one bound that is not part of that set, and deliberately. Every
other constraint lets keys be seen and passed over, so the cursor advances past
them and changing it strands objects. `--to` only stops the walk, in the same
direction the cursor moves, so nothing is ever passed over by it. Dropping it
is how a bounded run carries on:

```
metsuke-fetch sync --from 2026-08-28 --to 2026-09-02 --state range.json --into archive
metsuke-fetch sync --from 2026-08-28 --state range.json --into archive
```

Deleting a state file is the only rewind, and the run after it downloads
everything again: nothing checks whether an object is already on disk.

## The read

An object is a zstd stream whose first frame is skippable, so every conforming
zstd tool inflates the JSON Lines after it, and a downloaded tree reads where it
landed with no unpacking step. From a duckdb session:

```sql
select * from read_json('downloads/v1/*/*.jsonl.zst', sample_size=-1)
```

A metrics scrape and a trace line share no fields, so a read of both leaves
whichever it did not come from NULL. The kind is the last segment of every
object's name, which reads them apart without a column:

```sql
select * from read_json('downloads/v1/*/*-metrics.jsonl.zst', sample_size=-1)
select * from read_json('downloads/v1/*/*-logs.jsonl.zst', sample_size=-1)
```

`metsuke-fetch sync` prints the one of these that matches what it downloaded,
so the first read is usually a paste rather than something you write. It cannot
do that for a download directory whose own name holds `*`, `?` or `[`, which
are the glob's own characters. Name the directory without them.

Every line carries the pool and agent that wrote it under the `metsuke` key, so
a row selected out of any of these still says where it came from.

A scrape holds its metrics as a nested list and a trace line holds its payload
as a map, so neither reads usefully through `select *`. Two init files flatten
both, and each takes the directory to read from `$METSUKE_ARCHIVE`:

```
METSUKE_ARCHIVE=downloads duckdb -init docs/analytics.sql
METSUKE_ARCHIVE=downloads duckdb -init docs/archive.sql downloads.duckdb
```

`docs/analytics.sql` defines the views a consumer actually groups over. Being
views, they follow the root: `set variable archive = 'edge-1'` from the prompt
re-points them without reloading. Do not name a database file for this one. The
views persist into it and the variable does not, so reopening it answers
`read_json cannot take NULL list as parameter` until the variable is set again.

`docs/archive.sql` is the same flattening with none of the views, as three
tables over whatever directory you point it at. Its root has to arrive before
it runs, since the tables are built as it loads. Name a database file as above
and they persist, so later questions open `duckdb downloads.duckdb` and ask,
rather than re-reading every object.

Load that one where the question is not one analytics.sql already answers. It
globs recursively rather than assuming the `v1/<day>/` layout, unions objects
whose keys differ across days, and keeps a `scrape` row for every submission
including the failed ones, which carry no metrics and so cannot appear in the
flattened `metric` table.

Once one is loaded, `show tables` lists what it defined. `scrape`, `metric` and
`trace` are the flattened objects; the rest are the analytics:

```
counter_reset  coverage  eb_lifecycle  forge_scoreboard  log_cost
metric  mover  peer_activity  rts_pressure  scrape  trace
```

Each is a plain relation, so `select * from log_cost` is the whole of using
one, and `describe coverage` names its columns. To see what one is doing,
`select sql from duckdb_views() where view_name = 'log_cost'` prints its
definition. The comment above each in `docs/analytics.sql` says what it is for,
and `counter_reset` in particular is meant to come back empty: a rate computed
across a row it returns spans a node restart and is wrong.

Three of them are a good first look at an archive you have not seen before.
`log_cost` is what it is being spent on, by namespace, with each one's share.
`coverage` is the gap between consecutive scrapes per agent, against the
cadence one was configured for. `forge_scoreboard` is one row per scrape of the
block producer's counters, with the deltas between scrapes beside them.

## What a gap in an agent's sequence numbers is

What a gap in `counter` says, and what it does not, is in
[CONTEXT.md](../CONTEXT.md) under **Sequence Number**.

**From the archive alone a gap cannot be resolved, and no read here will do
it.** The submission that spent the number was refused, so it never became an
object: there is nothing under the missing counter to compare the next one
against. What the bucket holds is what landed, by
[ADR 0005](adr/0005-archive-raw-signed-bytes.md), and an attempt that did not
land is recorded only in the journal of the agent that made it.

Whoever holds that journal can resolve it. Every line the agent logs about a
submission names its payload digest, which covers the rows alone, so a refused
attempt and the later one carrying the same rows answer the same even though
their counters, bytes and signatures all differ. It is over exactly what
`zstd -d` hands back, so it is recomputable from the object that did land:

```
zstd -dc <object>.jsonl.zst | b2sum -l 64
```

Matching that against the digest the refusal logged is what shows the rows were
not lost. That correlates a journal to the bucket, so it is available to an
operator auditing their own agent, and not to a consumer reading an archive
whose agents are somebody else's. For that reader, gaps are not evidence of
anything and coverage is the timestamps below.

What was collected is what the lines themselves say, and the two kinds say it
differently, because they are collected differently. A scrape is one read of
the metrics endpoint on `scrape_interval_secs`, so metrics arrive on a cadence
you can expect a row for. Trace lines are not on that cadence at all: the agent
spools each one as the node writes it, so their rate is the node's and a quiet
stretch is the node being quiet.

Metrics carry the agent's own `scraped_at`, and a scrape that failed is still a
scrape, stored with a `failure` and no metrics. So a window with no row is the
only absence, and a gap against the cadence is what to look for:

```sql
select metsuke.pool_id, metsuke.agent_id, min(scraped_at), max(scraped_at), count(*)
from read_json('downloads/v1/*/*-metrics.jsonl.zst', sample_size=-1)
group by 1, 2
```

Trace lines carry no field of metsuke's beyond the `metsuke` stamp. Their time
is the node's own `at`, which is when the node emitted the line rather than
when the agent shipped it:

```sql
select metsuke.pool_id, metsuke.agent_id, ns, min(at), max(at), count(*)
from read_json('downloads/v1/*/*-logs.jsonl.zst', sample_size=-1)
group by 1, 2, 3
```

Neither read is a count of what the archive should have held. The agent drops
the oldest trace lines once `log_max_bytes` is reached, which is what it says
in the journal when it happens, so an absence there is a spool that could not
keep up rather than a node that said nothing.

## Why `sample_size=-1`

duckdb infers the JSON schema from the first 20480 rows and fills any field it
did not see there with NULL, in silence. A trace line from a namespace rarer
than that window is the case that costs: measured on duckdb 1.5.5 over a
25000-line file holding one such line, the default read gave that line's `data`
fields back as NULL, and `sample_size=-1` gave them back whole. The same window
drops a key added inside the `metsuke` stamp.

`read_json_auto` is that default under another name, so it carries the same
trap. The price of `-1` is one pass over the corpus before the first row.
