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

A scrape holds its metrics as a nested list and a trace line holds its payload
as a map, so neither reads usefully through `select *`. `duckdb -init
docs/analytics.sql` flattens both and defines the views a consumer actually
groups over.

## Why `sample_size=-1`

duckdb infers the JSON schema from the first 20480 rows and fills any field it
did not see there with NULL, in silence. A trace line from a namespace rarer
than that window is the case that costs: measured on duckdb 1.5.5 over a
25000-line file holding one such line, the default read gave that line's `data`
fields back as NULL, and `sample_size=-1` gave them back whole. The same window
drops a key added inside the `metsuke` stamp.

`read_json_auto` is that default under another name, so it carries the same
trap. The price of `-1` is one pass over the corpus before the first row.
