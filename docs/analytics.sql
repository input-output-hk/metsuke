-- Views over a directory `metsuke-fetch sync --into` wrote. Load with
--   duckdb -init docs/analytics.sql
-- and override the root first if it is not ./into:
--   set variable archive = 'downloads';
--
-- `select *` over the raw objects is not a useful read: a scrape holds its
-- metrics as a nested list, and a trace line holds its payload as a map. The
-- views below flatten both, so every question after this is one GROUP BY.
-- docs/reading-the-archive.md explains sample_size=-1 and the name globs.

set variable archive = 'into';

-- One row per scrape, metrics still nested.
create or replace view scrape as
select scraped_at::timestamptz as t,
       clock_offset_ms,
       failure,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent,
       metrics
from read_json(getvariable('archive') || '/v1/*/*-metrics.jsonl.zst', sample_size=-1);

-- One row per metric sample. This is the table to group over.
create or replace view metric as
select t, pool, agent, u.name, u.labels, u.value, u.declared_type
from scrape, unnest(metrics) as _(u);

-- One row per trace line. `data` is map(varchar, json): data['ebHash'].
create or replace view trace as
select "at"::timestamptz as t,
       ns, sev, thread, host, data,
       metsuke.pool_id as pool,
       metsuke.agent_id as agent
from read_json(getvariable('archive') || '/v1/*/*-logs.jsonl.zst', sample_size=-1);

-- Did the agent cover the window it claims to? A gap_s far off the configured
-- scrape interval is a missed upload, not a slow node.
create or replace view coverage as
select pool, agent, t,
       datediff('second', lag(t) over (partition by pool, agent order by t), t) as gap_s,
       clock_offset_ms,
       failure
from scrape;

-- The metrics worth plotting. A cardano-node scrape is mostly gauges pinned at
-- zero for the whole run; those are noise in every join and chart.
create or replace view mover as
select name, any_value(declared_type) as declared_type,
       min(value) as lo, max(value) as hi, count(distinct value) as distinct_values
from metric group by name having count(distinct value) > 1;

-- Counters are cumulative and reset on node restart. Rates computed across a
-- restart are wrong, so check this is empty before trusting any delta below.
create or replace view counter_reset as
select t, pool, agent, name, prev, value
from (select t, pool, agent, name, value,
             lag(value) over (partition by pool, agent, name order by t) as prev
      from metric where declared_type = 'counter')
where value < prev;

-- The block producer's scoreboard, one row per scrape.
create or replace view forge_scoreboard as
with pivoted as (
  select t, pool, agent,
    max(value) filter (where name = 'cardano_node_metrics_Forge_node_is_leader_counter') as leader,
    max(value) filter (where name = 'cardano_node_metrics_Forge_forged_counter')         as forged,
    max(value) filter (where name = 'cardano_node_metrics_Forge_adopted_counter')        as adopted,
    max(value) filter (where name = 'cardano_node_metrics_Forge_didnt_adopt_counter')    as didnt_adopt,
    max(value) filter (where name = 'cardano_node_metrics_slotsMissed_int')              as slots_missed,
    max(value) filter (where name = 'cardano_node_metrics_blockNum_int')                 as block_num,
    max(value) filter (where name = 'cardano_node_metrics_slotNum_int')                  as slot_num,
    max(value) filter (where name = 'cardano_node_metrics_density_real')                 as density,
    max(value) filter (where name = 'cardano_node_metrics_Mem_resident_int') / 1048576   as rss_mb,
    max(value) filter (where name = 'cardano_node_metrics_txsProcessedNum_int')          as txs_processed
  from metric group by 1, 2, 3)
select *,
       block_num - lag(block_num) over w as blocks_gained,
       slot_num  - lag(slot_num)  over w as slots_elapsed,
       forged    - lag(forged)    over w as forged_delta,
       leader    - lag(leader)    over w as leader_delta
from pivoted window w as (partition by pool, agent order by t);

-- Where CPU went between two scrapes: mutator vs GC, in wall ms.
create or replace view rts_pressure as
with pivoted as (
  select t, pool, agent,
    max(value) filter (where name = 'rts_gc_wall_ms')         as wall_ms,
    max(value) filter (where name = 'rts_gc_gc_wall_ms')      as gc_wall_ms,
    max(value) filter (where name = 'rts_gc_mutator_wall_ms') as mut_wall_ms,
    max(value) filter (where name = 'rts_gc_num_gcs')         as num_gcs,
    max(value) filter (where name = 'rts_gc_current_bytes_used') / 1048576 as live_mb,
    max(value) filter (where name = 'rts_gc_max_bytes_slop')  / 1048576 as max_slop_mb
  from metric group by 1, 2, 3)
select t, pool, agent, live_mb, max_slop_mb,
       gc_wall_ms - lag(gc_wall_ms) over w as gc_ms,
       wall_ms    - lag(wall_ms)    over w as elapsed_ms,
       num_gcs    - lag(num_gcs)    over w as gcs,
       100.0 * (gc_wall_ms - lag(gc_wall_ms) over w)
             / nullif(wall_ms - lag(wall_ms) over w, 0) as gc_pct
from pivoted window w as (partition by pool, agent order by t);

-- Every endorsement block this node saw, and what happened to it. A row where
-- announced_at is null but forged_at is not is an EB that never left.
create or replace view eb_lifecycle as
with ev as (
  select coalesce(data['ebHash'], data['hash'])::varchar as eb, ns, t, pool, agent, data
  from trace where map_contains(data, 'ebHash') or ns like 'Consensus.LeiosKernel.Block%')
select eb, pool, agent,
  min(t) filter (where ns = 'Consensus.LeiosKernel.BlockForged')          as forged_at,
  min(t) filter (where ns = 'Consensus.LeiosKernel.BlockAnnounced')       as announced_at,
  min(t) filter (where ns = 'Consensus.LeiosKernel.AnnouncementAccepted') as accepted_at,
  min(t) filter (where ns = 'Consensus.LeiosKernel.BlockAcquired')        as acquired_at,
  min(t) filter (where ns = 'Consensus.LeiosKernel.BlockCertified')       as certified_at,
  any_value(data['reason']) filter (where ns = 'Consensus.LeiosKernel.NotVoted') as not_voted_reason,
  bool_or(ns = 'Consensus.LeiosKernel.BlockPointMissing')                 as point_missing,
  any_value(data['announcementAgeSeconds']::double)
    filter (where ns = 'Consensus.LeiosKernel.AnnouncementAccepted')      as announcement_age_s,
  any_value(data['ebBodySize']::bigint)
    filter (where ns = 'Consensus.LeiosKernel.AnnouncementAccepted')      as eb_body_size
from ev group by 1, 2, 3;

-- What each upstream peer delivered, by connection.
create or replace view peer_activity as
select pool, agent, data['peer']->>'$.connectionId' as connection_id,
       count(*) as announcements,
       count(distinct data['ebHash']::varchar) as distinct_ebs,
       min(t) as first_seen, max(t) as last_seen
from trace where ns = 'Consensus.LeiosPeer.Announcement'
group by 1, 2, 3;

-- What the archive is spent on. Consensus.LeiosPeer.Msg carries a Haskell Show
-- rendering with no queryable structure, so its share here is the cost of
-- keeping a namespace nothing downstream can group by.
create or replace view log_cost as
select ns, count(*) as lines, sum(length(data::varchar)) as payload_bytes,
       round(100.0 * sum(length(data::varchar))
             / sum(sum(length(data::varchar))) over (), 1) as pct
from trace group by 1;
