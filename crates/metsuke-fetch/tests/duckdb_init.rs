//! The two duckdb init files the analysis page hands a consumer, run over an
//! archive this tool synced.
//!
//! They are the documented way the telemetry is read, so a claim either of
//! them makes about what it loads is a claim about the numbers somebody
//! computes from it. Nothing else executes them: they are `include_str!`'d
//! into the server and served, and being served is not being run.
//!
//! The archive is a real sync of real sealed objects rather than a hand-built
//! tree, so what the SQL reads is the layout, the compression and the
//! envelopes the tool actually produces.

use std::path::{Path, PathBuf};
use std::process::Command;

use metsuke_fetch::select::{Days, Filters, Selection};
use metsuke_fetch::sync::{self, Destination, Insist, Verification};
use metsuke_wire::key::KEY_PREFIX;
use std::num::NonZeroU64;

mod support;
use support::Server;

/// Where the init files live, from this crate rather than from a copy: the
/// files the server serves are these.
fn sql(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs")
        .join(name)
}

/// An archive of `count` objects on disk, synced the way an operator would.
fn synced_archive(count: usize) -> (tempfile::TempDir, PathBuf) {
    let server = Server::attesting(count, 100);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("into");
    let selection = Selection::default();
    let days = Days::default();
    let prefix = KEY_PREFIX.to_string();
    let report = sync::run(
        &server.pulling(),
        &Filters {
            prefix: &prefix,
            selection: &selection,
            days: &days,
        },
        &Destination {
            into: &into,
            state: &dir.path().join("cursor.json"),
        },
        Verification {
            max_object_bytes: NonZeroU64::new(1 << 20).unwrap(),
            insist: Insist::Nothing,
        },
        |_| {},
    )
    .expect("the archive syncs");
    assert_eq!(report.objects as usize, count, "{report:?}");
    (dir, into)
}

/// Run `archive.sql` into a database file and hand back what it said while
/// doing it, which is where a statement it stepped over is reported
/// (`.bail off`).
///
/// A file and not memory, because that is what the header tells a reader to
/// do and what makes the query below a second session with no init file: the
/// tables are the whole state, so nothing about them depends on how they were
/// built.
fn load(archive: &Path, database: &Path) -> String {
    let loaded = Command::new("duckdb")
        .env("METSUKE_ARCHIVE", archive)
        .args(["-init"])
        .arg(sql("archive.sql"))
        .arg(database)
        .args(["-c", "select 1"])
        .output()
        .expect("duckdb runs (flake.nix suiteTools)");
    assert!(
        loaded.status.success(),
        "archive.sql: {}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    String::from_utf8_lossy(&loaded.stderr).to_string()
}

/// One answer out of a database the tables are already in.
fn count(database: &Path, query: &str) -> u64 {
    let answered = Command::new("duckdb")
        .args(["-noheader", "-list"])
        .arg(database)
        .args(["-c", query])
        .output()
        .expect("duckdb runs");
    assert!(
        answered.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&answered.stderr)
    );
    String::from_utf8_lossy(&answered.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|_| {
            panic!(
                "{query} answered {:?}",
                String::from_utf8_lossy(&answered.stdout)
            )
        })
}

/// One answer out of `analytics.sql`, which builds views rather than tables,
/// so the query has to run in the session that read the init file: a view
/// names the archive through a variable a later session would not have.
fn view(archive: &Path, query: &str) -> (u64, String) {
    let answered = Command::new("duckdb")
        .env("METSUKE_ARCHIVE", archive)
        .args(["-noheader", "-list", "-init"])
        .arg(sql("analytics.sql"))
        .args(["-c", query])
        .output()
        .expect("duckdb runs");
    assert!(
        answered.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&answered.stderr)
    );
    let stderr = String::from_utf8_lossy(&answered.stderr).to_string();
    let out = String::from_utf8_lossy(&answered.stdout).trim().to_string();
    (
        out.parse()
            .unwrap_or_else(|_| panic!("{query} answered {out:?}, said {stderr}")),
        stderr,
    )
}

/// All three tables `archive.sql` says it builds, over an archive holding both
/// kinds of object.
///
/// `.bail off` means a statement that fails is reported and stepped over, so
/// stderr is asserted as well. What proves each table built is its own count
/// below, one per table the file creates; the stderr check is what catches a
/// statement that failed without changing any of them.
#[test]
fn archive_sql_builds_all_three_tables_over_a_synced_archive() {
    let (dir, archive) = synced_archive(6);
    let database = dir.path().join("archive.duckdb");

    let said = load(&archive, &database);

    assert!(!said.contains("Error"), "archive.sql: {said}");
    // Three objects of each kind, one row in each from each.
    assert_eq!(count(&database, "select count(*) from scrape"), 3);
    assert_eq!(count(&database, "select count(*) from trace"), 3);
    // The unnest of the scrapes, so one row per metric sample rather than per
    // object. The fixture's scrape carries one, and `analytics_sql_answers_
    // over_a_synced_archive` reads the same number out of the view.
    assert_eq!(count(&database, "select count(*) from metric"), 3);
    // What the tables are for: the columns the flatten exists to expose.
    assert_eq!(
        count(
            &database,
            "select count(*) from metric where name is not null and value is not null"
        ),
        count(&database, "select count(*) from metric")
    );
    assert_eq!(
        count(
            &database,
            "select count(*) from trace where ns like 'Consensus.%' and data is not null"
        ),
        3
    );
}

/// The dedup both files claim: the same submission stored twice reaches the
/// archive as two objects, and one scrape is one row.
#[test]
fn archive_sql_counts_a_resealed_submission_once() {
    let (dir, archive) = synced_archive(2);
    let one = std::fs::read_dir(archive.join("v1"))
        .expect("the tree has a day folder")
        .filter_map(Result::ok)
        .flat_map(|day| std::fs::read_dir(day.path()).expect("a day holds objects"))
        .filter_map(Result::ok)
        .map(|object| object.path())
        .find(|path| path.to_string_lossy().contains("-metrics.jsonl.zst"))
        .expect("the archive holds a metrics object");

    // What the server does when a PUT succeeded with the response lost: the
    // same bytes under a fresh key (ADR 0005 keeps what landed).
    let again = one.with_file_name(format!(
        "resealed-{}",
        one.file_name().expect("a name").to_string_lossy()
    ));
    std::fs::copy(&one, &again).expect("the copy writes");

    let database = dir.path().join("archive.duckdb");
    load(&archive, &database);

    assert_eq!(
        count(&database, "select count(*) from scrape"),
        1,
        "the resealed copy was counted again"
    );
    // And the same over the views, which pay for the dedup per query.
    assert_eq!(view(&archive, "select count(*) from scrape").0, 1);
}

/// The views `analytics.sql` answers particular questions with, over the same
/// archive. Views rather than tables, so this is also what says the glob and
/// the flatten still line up with what a sync writes.
#[test]
fn analytics_sql_answers_over_a_synced_archive() {
    let (_dir, archive) = synced_archive(6);

    // Every view the file defines, so one that stopped binding is caught here
    // rather than by the consumer it was written for.
    for name in [
        "scrape",
        "metric",
        "trace",
        "coverage",
        "mover",
        "counter_reset",
        "forge_scoreboard",
        "rts_pressure",
        "eb_lifecycle",
        "peer_activity",
        "log_cost",
    ] {
        let query = format!("select count(*) from {name}");
        let (rows, said) = view(&archive, &query);
        assert!(!said.contains("Error"), "{name}: {said}");
        // Not a row count: several of these are empty over this fixture, and
        // what is asserted is that the view binds and answers.
        let _ = rows;
    }
    // The three that this archive does have rows for, so the walk above
    // cannot pass on a file whose views all answer nothing.
    for name in ["scrape", "metric", "trace"] {
        let query = format!("select count(*) from {name}");
        assert_eq!(view(&archive, &query).0, 3, "{name}");
    }
}
