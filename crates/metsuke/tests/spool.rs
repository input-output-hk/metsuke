//! Spool durability tests (ticket metsuke-4zo.4): nothing is lost across
//! restarts or server downtime, and rows leave only through ACK.

use metsuke::spool::{Spool, SpoolConfig};
use metsuke_wire::envelope::Sample;
use proptest::prelude::*;
use time::OffsetDateTime;

fn sample_at(unix_secs: i64) -> Sample {
    Sample {
        sampled_at: OffsetDateTime::from_unix_timestamp(unix_secs).unwrap(),
        block_height: Some(unix_secs as u64),
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}

fn temp_config(dir: &tempfile::TempDir, max_samples: u64) -> SpoolConfig {
    SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_samples,
    }
}

#[test]
fn undelivered_rows_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, 100);
    let samples = [sample_at(1), sample_at(2)];
    {
        let mut spool = Spool::open(&config).unwrap();
        for sample in &samples {
            spool.push(sample).unwrap();
        }
    }
    let reopened = Spool::open(&config).unwrap();
    let rows = reopened.outstanding().unwrap();
    let offered: Vec<_> = rows.into_iter().map(|row| row.sample).collect();
    assert_eq!(offered, samples);
}

#[test]
fn ack_deletes_only_the_acked_rows() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 100)).unwrap();
    for secs in 1..=3 {
        spool.push(&sample_at(secs)).unwrap();
    }
    let rows = spool.outstanding().unwrap();
    spool.ack(&[rows[0].id, rows[1].id]).unwrap();
    let remaining = spool.outstanding().unwrap();
    assert_eq!(remaining, vec![rows[2].clone()]);
}

// ADR 0004: an unreachable server drops oldest data, never fills the disk.
#[test]
fn size_cap_drops_oldest_on_push() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 3)).unwrap();
    for secs in 1..=5 {
        spool.push(&sample_at(secs)).unwrap();
    }
    let offered: Vec<_> = spool
        .outstanding()
        .unwrap()
        .into_iter()
        .map(|row| row.sample)
        .collect();
    assert_eq!(offered, [sample_at(3), sample_at(4), sample_at(5)]);
}

// A counter value handed out must never be handed out again, even across a
// restart — the server rejects reuse as replay (ADR 0002).
#[test]
fn replay_counter_is_monotonic_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, 100);
    let before_restart = {
        let mut spool = Spool::open(&config).unwrap();
        let first = spool.next_counter().unwrap();
        let second = spool.next_counter().unwrap();
        assert!(second > first);
        second
    };
    let mut reopened = Spool::open(&config).unwrap();
    assert!(reopened.next_counter().unwrap() > before_restart);
}

// Migrations run on open: a version-0 SQLite file someone else created (here
// a raw empty DB) comes out with the working schema, and its recorded schema
// version is 1 — the one released migration.
#[test]
fn open_migrates_a_fresh_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = temp_config(&dir, 100);
    rusqlite::Connection::open(&config.path).unwrap();
    let mut spool = Spool::open(&config).unwrap();
    spool.push(&sample_at(1)).unwrap();
    assert_eq!(spool.outstanding().unwrap().len(), 1);
    let raw = rusqlite::Connection::open(&config.path).unwrap();
    let version: u32 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
}

proptest! {
    // ADR 0004: rows leave only through ACK, and an ACK deletes exactly the
    // acked rows. After random push/partial-ack interleavings, acking every
    // outstanding row leaves zero sample rows — checked through the API and
    // again on the raw file, so a lying `outstanding()` can't hide orphans.
    #[test]
    fn write_ack_delete_leaves_no_orphan_rows(
        batches in prop::collection::vec((1usize..8, any::<prop::sample::Index>()), 1..10),
        cap in 1u64..50,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let config = temp_config(&dir, cap);
        let mut spool = Spool::open(&config).unwrap();
        let mut pushed = 0i64;
        for (count, ack_pick) in batches {
            for _ in 0..count {
                spool.push(&sample_at(pushed)).unwrap();
                pushed += 1;
            }
            let rows = spool.outstanding().unwrap();
            let acked: Vec<i64> = rows[..ack_pick.index(rows.len() + 1)]
                .iter()
                .map(|row| row.id)
                .collect();
            spool.ack(&acked).unwrap();
            let remaining = spool.outstanding().unwrap();
            prop_assert!(remaining.iter().all(|row| !acked.contains(&row.id)));
        }
        let all: Vec<i64> = spool.outstanding().unwrap().iter().map(|row| row.id).collect();
        spool.ack(&all).unwrap();
        prop_assert!(spool.outstanding().unwrap().is_empty());
        drop(spool);
        let raw = rusqlite::Connection::open(&config.path).unwrap();
        let orphans: i64 = raw
            .query_row("SELECT count(*) FROM samples", [], |row| row.get(0))
            .unwrap();
        prop_assert_eq!(orphans, 0);
    }
}

#[test]
fn pushed_sample_is_outstanding() {
    let dir = tempfile::tempdir().unwrap();
    let mut spool = Spool::open(&temp_config(&dir, 100)).unwrap();
    let sample = sample_at(1_000);
    spool.push(&sample).unwrap();
    let rows = spool.outstanding().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].sample, sample);
}
