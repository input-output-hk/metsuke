# Reading the archive

What a consumer runs over a directory `metsuke-fetch sync --into` wrote, and
why that read is not the obvious one.

## The read

An object is a zstd stream whose first frame is skippable, so every conforming
zstd tool inflates the JSON Lines after it and duckdb reads a downloaded tree
where it landed, with no unpacking step:

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

`metsuke-fetch sync` prints the one of these that matches what it downloaded.
It cannot print a usable one for a download directory whose own name holds a
glob character. `*` and `[` survive being bracketed, and on duckdb 1.5.5 a
directory named `q?m` read back through `q[?]m` handed the compressed bytes to
the JSON parser. Name the directory without them.

Every line carries the pool and agent that wrote it under the `metsuke` key, so
a row selected out of any of these still says where it came from.

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

An object that arrives without those headers cannot be checked at all. That is
what a filesystem archive answers, having discarded the pair at ingest rather
than merely not serving it, and what any object something other than this
server wrote answers. Bytes without them are bytes the server is asking to be
believed about.

`metsuke-fetch sync` runs both halves on every object as it lands, so a
directory it wrote holds nothing that failed. Its summary says how many of each
it saw:

```
3 objects into ~/archive, 3145728 bytes; 3 verified, 0 unverifiable, 0 not written; …
```

An object that fails is named on stderr, is not written, and makes the run exit
nonzero. The cursor still advances past it, because the archive is append-only
and the same bytes will fail the same way tomorrow; syncing it again means
rewinding the state file. What to do about one is not this tool's:
`metsuke-server verify-archive` walks the bucket and names every object whose
stored bytes and metadata disagree, and removing one is a bucket-admin action,
since the server holds no delete.

Unverifiable is counted rather than refused, or a run against a filesystem
archive would download nothing. `--require-verified` turns it into a refusal,
which is what a consumer computing anything that matters should pass.

## What a gap in an agent's counters is

`counter` runs per agent, and a value is never handed out twice: an agent draws
and persists one before it seals, so an attempt the server refused has spent
its number and the same lines are sent again under a later one. A rate limit
met mid-drain is the ordinary case, and it leaves a hole.

So a gap says a submission did not land. It does not say anything was lost, it
is not a count of what is missing, and summing gaps to measure coverage counts
every retry as a loss.

**From the archive alone a gap cannot be resolved, and no read here will do
it.** The submission that spent the number was refused, so it never became an
object: there is nothing under the missing counter to compare the next one
against. What the bucket holds is what landed, by ADR 0005, and an attempt that
did not land is recorded only in the journal of the agent that made it.

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
select metsuke.agent_id, min(scraped_at), max(scraped_at), count(*)
from read_json('downloads/v1/*/*-metrics.jsonl.zst', sample_size=-1)
group by 1
```

Trace lines carry no field of metsuke's beyond the `metsuke` stamp. Their time
is the node's own `at`, which is when the node emitted the line rather than
when the agent shipped it:

```sql
select metsuke.agent_id, ns, min(at), max(at), count(*)
from read_json('downloads/v1/*/*-logs.jsonl.zst', sample_size=-1)
group by 1, 2
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
