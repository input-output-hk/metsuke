-- Three tables over any directory holding metsuke archive objects, whatever
-- its shape.
--
-- Name a database file, or the tables are built in memory and thrown away when
-- you exit, which means re-reading every object for each question:
--   METSUKE_ARCHIVE=edge-1 duckdb -init docs/archive.sql edge-1.duckdb
-- and afterwards, with the tables already there and no init file:
--   duckdb edge-1.duckdb
--
-- Which directory is read comes from the first of: an `archive` variable
-- already set, $METSUKE_ARCHIVE, then ./into. Setting the variable works from
-- the prompt, before `.read`ing this file. It does not work as
-- `duckdb -c "set variable archive = …" -init`, because the init file runs
-- first and would build the tables before the -c arrived.
--
-- Tables rather than views, so each glob is resolved once and repeated queries
-- do not re-read the zstd. Re-run the file after a new sync.
--
-- If you fetched only one kind, the CREATE for the other finds no files and
-- says so. That is harmless, and `.bail off` below is what makes it harmless:
-- duckdb otherwise stops reading an init file at the first error and exits,
-- leaving no session at all. The tables are built in order, so what did match
-- is loaded and usable, and the summary at the end reports each table
-- separately rather than as one query that a missing table would take down.
--
-- docs/analytics.sql is the other half of this: views that answer particular
-- questions about a cardano-node archive. This file answers none, and is the
-- one to load when the question is not one of those.

-- Keep going after a statement that fails, which an init file otherwise does
-- not. It is what lets a tree holding one kind still load that kind. The cost
-- is that a genuine mistake in this file is reported and stepped over rather
-- than stopping it, so read what it prints.
.bail off

-- nullif, because getenv answers an unset variable with the empty string
-- rather than NULL, and coalesce would take it and read the filesystem root.
set variable archive =
  coalesce(getvariable('archive'), nullif(getenv('METSUKE_ARCHIVE'), ''), 'into');

-- '**' matches zero directories as well as many, so this reads the v1/<day>/
-- tree metsuke-fetch writes and an unnested pile of objects equally.

-- One row per scrape, failures included. A failed scrape carries no metrics at
-- all, so it disappears from the flattened table below; this is where it still
-- exists, and `where failure is not null` is how you find the gaps.
create or replace table scrape as
select scraped_at::timestamptz as t,
       clock_offset_ms,
       failure,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent,
       metrics
from read_json(getvariable('archive') || '/**/*-metrics.jsonl.zst',
               sample_size = -1, union_by_name = true);

-- One row per metric sample. The table to group over.
create or replace table metric as
select t, pool, agent, u.name, u.labels, u.value, u.declared_type
from scrape, unnest(metrics) as _(u);

-- The nested copy is now redundant and is the bulk of `scrape`'s footprint.
alter table scrape drop column metrics;

-- One row per trace line. `data` is map(varchar, json): data['ebHash'].
create or replace table trace as
select "at"::timestamptz as t,
       ns, sev, thread, host, data,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent
from read_json(getvariable('archive') || '/**/*-logs.jsonl.zst',
               sample_size = -1, union_by_name = true);

-- What loaded, so a glob that matched nothing says so at once rather than as
-- an empty result three queries later.
-- One statement each rather than one union: a table the tree held no files for
-- was never created, and a union naming it reports nothing about the two that
-- did load.
select 'scrape' as "table", count(*) as rows,
       min(t) as first, max(t) as last,
       count(distinct pool) as pools, count(distinct agent) as agents
from scrape;
select 'metric' as "table", count(*) as rows,
       min(t) as first, max(t) as last,
       count(distinct pool) as pools, count(distinct agent) as agents
from metric;
select 'trace' as "table", count(*) as rows,
       min(t) as first, max(t) as last,
       count(distinct pool) as pools, count(distinct agent) as agents
from trace;
