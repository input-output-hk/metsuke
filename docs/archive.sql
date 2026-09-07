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
-- A table that failed to build is absent rather than left over from the run
-- before, so the summary is short a line instead of naming rows that are not
-- this run's. Read what it prints either way: the errors are above it.
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
--
-- One row per scrape and not per stored copy of it. A submission whose PUT
-- succeeded with the response lost is resealed and uploaded again under a
-- fresh key, and a replay inside the skew window is stored a second time too,
-- so the same scrape reaches the archive as two objects. Nothing on the server
-- deduplicates that (ADR 0005 keeps what landed), and counting the copies
-- reads a pool with a flaky uplink as a more productive one. Measured on a
-- real archive: 11 of 1128 scrape rows, each in two distinct objects.
--
-- An agent reads the endpoint once per interval, so pool, agent and the
-- agent's own scraped_at name the scrape rather than the upload. To count the
-- copies instead, read the objects with filename=true and group by that.
-- Dropped before it is built, and `create or replace` is not enough on its
-- own: the replace happens only if the select succeeds, so over a database
-- from an earlier run a create that fails leaves the earlier table sitting
-- there and the summary below reports its rows as this run's. Re-running the
-- file after a new sync is the documented steady state, so that is the
-- ordinary path, not a corner. Dropped first, a failed build leaves no table
-- and the summary says so.
drop table if exists scrape;
create or replace table scrape as
select scraped_at::timestamptz as t,
       clock_offset_ms,
       failure,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent,
       metrics
from read_json(getvariable('archive') || '/**/*-metrics.jsonl.zst',
               sample_size = -1, union_by_name = true)
qualify row_number() over (partition by pool, agent, t) = 1;

-- One row per metric sample. The table to group over.
drop table if exists metric;
create or replace table metric as
select t, pool, agent, u.name, u.labels, u.value, u.declared_type
from scrape, unnest(metrics) as _(u);

-- The nested copy is now redundant and is the bulk of `scrape`'s footprint.
alter table scrape drop column metrics;

-- One row per trace line, deduplicated for the reason `scrape` is. The whole
-- line is the key here, because a trace line carries no field of the agent's
-- to name it by: the node's `at`, its namespace and its payload together. That
-- `at` is compared to the microsecond, which is all `timestamptz` holds of the
-- nanoseconds the node wrote, so the window is that wide. Two distinct events
-- inside one microsecond agreeing on namespace and payload is still not a
-- thing a node does. Measured on the same archive: 53 of 3064880.
-- `data` as JSON and not as read_json inferred it, for the reason
-- docs/analytics.sql gives at its own `trace`: inference gives a struct of
-- whichever fields the objects in front of it carried, so reading a field that
-- is absent is an error rather than a null. `data->>'$.ebHash'` for a value,
-- `json_exists(data, '$.ebHash')` for whether it is there.
drop table if exists trace;
create or replace table trace as
select "at"::timestamptz as t,
       ns, sev, thread, host, data::json as data,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent
from read_json(getvariable('archive') || '/**/*-logs.jsonl.zst',
               sample_size = -1, union_by_name = true)
qualify row_number() over (partition by pool, agent, t, ns, data::varchar) = 1;

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
