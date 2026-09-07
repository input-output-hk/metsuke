//! What a sync has to hold, against the real server: the bytes on disk are the
//! archived object, an interrupted run resumes rather than restarts, and the
//! filters select without downloading.

use std::path::Path;
use std::process::Command;

use metsuke_fetch::cursor::Cursor;

use metsuke_fetch::recipe;
use metsuke_fetch::select::{Days, Filters, Selection};
use metsuke_fetch::sync::{self, Destination, Insist, SyncError, Verification};
use metsuke_wire::envelope::{self, AgentId, Limits};
use metsuke_wire::http::Listing;
use metsuke_wire::key::{KEY_PREFIX, Kind, ObjectName};
use std::num::NonZeroU64;

mod support;
use support::{Server, other_key, pool_of, test_key};

/// One sync into a fresh directory, with the keys it reported.
#[derive(Debug)]
struct Synced {
    dir: tempfile::TempDir,
    landed: Vec<String>,
    report: sync::Report,
}

impl Synced {
    fn path(&self, key: &str) -> std::path::PathBuf {
        self.dir.path().join("objects").join(key)
    }
}

fn state_of(dir: &Path) -> std::path::PathBuf {
    dir.join("cursor.json")
}

/// What a run was asked for, owned so a test can hold it across two runs.
struct Asked {
    days: Days,
    prefix: String,
    selection: Selection,
}

impl Asked {
    fn filters(&self) -> Filters<'_> {
        Filters {
            prefix: &self.prefix,
            selection: &self.selection,
            days: &self.days,
        }
    }
}

/// The whole archive, which is what a run with no filter flags asks for
/// (`cli::Args`).
fn everything() -> Asked {
    Asked {
        prefix: KEY_PREFIX.to_string(),
        days: Days::default(),
        selection: Selection::default(),
    }
}

/// The whole archive narrowed to one selection.
fn only(selection: Selection) -> Asked {
    Asked {
        prefix: KEY_PREFIX.to_string(),
        days: Days::default(),
        selection,
    }
}

/// What a test that is not about the checking asks for: an object bound no
/// fixture reaches, and unattested objects counted rather than refused,
/// which is what a filesystem archive hands back.
fn permissive() -> Verification {
    Verification {
        max_object_bytes: NonZeroU64::new(1 << 20).unwrap(),
        insist: Insist::Nothing,
    }
}

/// Sync `server` under `asked` into `dir`, which a resuming test reuses.
fn sync_into(
    server: &Server,
    asked: &Asked,
    dir: tempfile::TempDir,
) -> Result<Synced, (Box<SyncError>, tempfile::TempDir, Vec<String>)> {
    sync_verifying(server, asked, dir, permissive())
}

fn sync_verifying(
    server: &Server,
    asked: &Asked,
    dir: tempfile::TempDir,
    verification: Verification,
) -> Result<Synced, (Box<SyncError>, tempfile::TempDir, Vec<String>)> {
    let into = dir.path().join("objects");
    let state = state_of(dir.path());
    let mut landed = Vec::new();
    let filters = asked.filters();
    let destination = Destination {
        into: &into,
        state: &state,
    };
    match sync::run(
        &server.pulling(),
        &filters,
        &destination,
        verification,
        |key| landed.push(key.to_string()),
    ) {
        Ok(report) => Ok(Synced {
            dir,
            landed,
            report,
        }),
        Err(error) => Err((Box::new(error), dir, landed)),
    }
}

fn synced(server: &Server, asked: &Asked) -> Synced {
    sync_into(server, asked, tempfile::tempdir().expect("a temp dir"))
        .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"))
}

/// The keys `list` reports under `asked`, with the report beside them.
fn listed(server: &Server, asked: &Asked) -> (Vec<String>, sync::Report) {
    let mut keys = Vec::new();
    let report = sync::list(&server.pulling(), &asked.filters(), |key| {
        keys.push(key.to_string())
    })
    .expect("the listing answers");
    (keys, report)
}

#[test]
fn a_downloaded_object_is_the_archived_bytes_and_still_verifies() {
    let server = Server::with_objects(1, 100);
    let object = &server.objects[0];

    let synced = synced(&server, &everything());

    let downloaded = std::fs::read(synced.path(&object.key)).expect("the object landed");
    assert_eq!(downloaded, object.wire_bytes);
    envelope::open(
        &object.attestation,
        &downloaded,
        Limits {
            max_header_bytes: 4096,
            max_decompressed_bytes: 1 << 20,
        },
    )
    .expect("the downloaded bytes open under the signature the archive holds");
    assert_eq!(
        synced.report,
        sync::Report {
            objects: 1,
            bytes: object.wire_bytes.len() as u64,
            passed: 0,
            unnameable: 0,
            // The suite's server stores to a filesystem archive, which
            // discards the pair at ingest, so nothing it serves can be checked
            // by anybody. `cold_signed` is the attesting server's to answer.
            cold_signed: 0,
            leios_signed: 0,
            unattested: 1,
            rejected: Vec::new(),
        }
    );
}

/// The whole point of the two headers: an archive that answers them is one
/// whose objects this tool checks for itself, rather than taking the server's
/// word for what it handed over.
#[test]
fn an_object_that_carries_its_key_and_signature_is_checked() {
    let server = Server::attesting(2, 100);

    let synced = synced(&server, &everything());

    assert_eq!(synced.report.cold_signed, 2);
    assert_eq!(synced.report.unattested, 0);
    assert_eq!(synced.report.rejected, Vec::new());
    for key in server.keys() {
        assert!(synced.path(&key).is_file(), "{key} did not land");
    }
}

/// Bytes that do not stand under the signature beside them are not written at
/// all. A reader globbing the download directory cannot pick up an object
/// nobody may trust, which is why the check happens before the file does.
#[test]
fn an_object_whose_bytes_do_not_verify_is_not_written() {
    let server = Server::attesting(1, 100);
    let key = server.keys()[0].clone();
    server.tamper(&key);

    let synced = synced(&server, &everything());

    assert_eq!(synced.report.cold_signed, 0);
    assert_eq!(synced.report.objects, 0);
    assert!(!synced.path(&key).exists(), "{key} must not be on disk");
    assert_eq!(synced.report.rejected.len(), 1, "{:?}", synced.report);
    assert_eq!(synced.report.rejected[0].key, key);
    assert!(
        synced.report.rejected[0].reason.contains("signature"),
        "got: {}",
        synced.report.rejected[0].reason
    );
    // Reported, and not handed to the caller as landed: that list is what a
    // reader will find.
    assert!(synced.landed.is_empty(), "got: {:?}", synced.landed);
}

/// The other half, and the one a signature check alone would pass: bytes a
/// stranger sealed, filed under a pool that never sent them. The key hashing
/// to the pool in the object's own name is what says whose it is.
#[test]
fn an_object_signed_by_another_pool_is_not_written() {
    let server = Server::attesting(1, 100);
    let key = server.keys()[0].clone();
    server.reseal_as(&key, &other_key());

    let synced = synced(&server, &everything());

    assert_eq!(synced.report.rejected.len(), 1, "{:?}", synced.report);
    assert!(
        synced.report.rejected[0].reason.contains("filed under"),
        "got: {}",
        synced.report.rejected[0].reason
    );
    assert!(!synced.path(&key).exists(), "{key} must not be on disk");
}

/// An archive that stores no metadata leaves every object unattested, and a
/// run against one still syncs: refusing by default would leave the tool
/// unable to read the archive a single-host deployment writes.
#[test]
fn an_object_with_nothing_to_check_it_by_lands_and_is_counted() {
    let server = Server::with_objects(1, 100);

    let synced = synced(&server, &everything());

    assert_eq!(synced.report.unattested, 1);
    assert_eq!(synced.report.cold_signed, 0);
    assert_eq!(synced.report.rejected, Vec::new());
    assert!(synced.path(&server.keys()[0]).is_file());
}

/// And what `--require-attested` is for: the consumer that needs the guarantee
/// says so, and then unattested is a refusal like any other.
#[test]
fn require_attested_refuses_an_object_with_nothing_to_check_it_by() {
    let server = Server::with_objects(1, 100);
    let asked = everything();

    let synced = sync_verifying(
        &server,
        &asked,
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            insist: Insist::Attested,
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));

    assert_eq!(synced.report.rejected.len(), 1, "{:?}", synced.report);
    assert!(
        synced.report.rejected[0]
            .reason
            .contains("unattested: no key and signature"),
        "got: {}",
        synced.report.rejected[0].reason
    );
    assert!(!synced.path(&server.keys()[0]).exists());
}

/// The bounds walk the listing rather than filter it. The suite seeds one
/// object per day, so bounding to the middle two leaves the first and last
/// unlisted, and `passed` stays zero: a key outside the range was never
/// listed, which is not the same as one the selection turned down.
#[test]
fn the_bounds_leave_a_key_outside_them_unlisted() {
    let server = Server::with_objects(4, 100);
    let days: Vec<String> = server
        .keys()
        .iter()
        .map(|key| {
            key.split('/')
                .nth(1)
                .expect("a key names its day")
                .to_string()
        })
        .collect();

    let (listed, report) = listed(
        &server,
        &Asked {
            days: Days {
                from: Some(format!("{KEY_PREFIX}{}", days[1])),
                until: Some(format!("{KEY_PREFIX}{}", days[3])),
            },
            ..everything()
        },
    );

    assert_eq!(listed, server.keys()[1..3], "{report:?}");
    assert_eq!(report.listed(), 2, "{report:?}");
    assert_eq!(report.passed, 0, "{report:?}");
}

/// A refused object increments none of the landing counts, so the listing
/// total has to add it back. Reported without that, a run that refused most of
/// what it selected said the prefix had listed only what landed.
#[test]
fn the_listing_total_counts_what_was_refused() {
    let server = Server::attesting(3, 100);
    let refused = server.keys()[0].clone();
    server.tamper(&refused);

    let synced = synced(
        &server,
        &only(Selection {
            kind: Some(Kind::Metrics),
            ..Selection::default()
        }),
    );

    let report = &synced.report;
    assert_eq!(report.rejected.len(), 1, "{report:?}");
    assert_eq!(report.selected(), report.objects + 1, "{report:?}");
    assert_eq!(
        report.listed(),
        report.selected() + report.passed + report.unnameable,
        "{report:?}"
    );
    // The archive holds three objects and every one of them was listed.
    assert_eq!(report.listed(), 3, "{report:?}");
}

/// A Leios key names no pool, so its object's filing is the server's word. Its
/// bytes are not: the signature is checked here like any other's, and counting
/// it beside an object that carried no signature at all would say the two were
/// equally unchecked.
#[test]
fn a_leios_signed_object_is_counted_apart_from_one_carrying_no_signature() {
    let server = Server::attesting(2, 100);
    let leios = server.keys()[0].clone();
    server.leios_sign(&leios);

    let synced = synced(&server, &everything());

    assert_eq!(synced.report.cold_signed, 1, "{:?}", synced.report);
    assert_eq!(synced.report.leios_signed, 1, "{:?}", synced.report);
    assert_eq!(synced.report.unattested, 0, "{:?}", synced.report);
    assert_eq!(synced.report.rejected, Vec::new());
    assert!(synced.path(&leios).is_file(), "{leios} did not land");
}

/// The flag a consumer wants once pools hold no cold key: the bytes are proven
/// either way, so both land and only what carries no signature is refused.
#[test]
fn require_attested_takes_a_leios_signed_object() {
    let server = Server::attesting(2, 100);
    let leios = server.keys()[0].clone();
    server.leios_sign(&leios);

    let synced = sync_verifying(
        &server,
        &everything(),
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            insist: Insist::Attested,
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));

    assert_eq!(synced.report.rejected, Vec::new(), "{:?}", synced.report);
    assert_eq!(synced.report.leios_signed, 1);
    assert!(synced.path(&leios).is_file(), "{leios} did not land");
}

/// The strict flag, which keeps only what stays checkable from the object
/// alone. The refusal says the signature stood, so it does not read as bad
/// bytes.
#[test]
fn require_cold_signed_refuses_a_leios_signed_object() {
    let server = Server::attesting(2, 100);
    let leios = server.keys()[0].clone();
    server.leios_sign(&leios);

    let synced = sync_verifying(
        &server,
        &everything(),
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            insist: Insist::ColdSigned,
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));

    assert_eq!(synced.report.rejected.len(), 1, "{:?}", synced.report);
    assert_eq!(synced.report.rejected[0].key, leios);
    assert!(
        synced.report.rejected[0]
            .reason
            .contains("Leios signature verified, not cold-signed"),
        "got: {}",
        synced.report.rejected[0].reason
    );
    assert!(!synced.path(&leios).exists(), "{leios} must not be on disk");
}

/// Checking an object means holding it, so the run states what it will hold.
/// Over that, the object is reported and the rest of the archive still syncs.
#[test]
fn an_object_over_the_bound_is_reported_and_the_sync_goes_on() {
    let server = Server::attesting(2, 100);
    let asked = everything();

    let synced = sync_verifying(
        &server,
        &asked,
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            max_object_bytes: NonZeroU64::new(1).unwrap(),
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));

    assert_eq!(synced.report.rejected.len(), 2, "{:?}", synced.report);
    assert!(
        synced.report.rejected[0].reason.contains("byte limit"),
        "got: {}",
        synced.report.rejected[0].reason
    );
}

/// Objects land under the key they are filed as, folders and all, which is what
/// a reader globbing `**/*.jsonl.zst` over the download directory needs.
#[test]
fn objects_land_under_the_keys_they_are_filed_as() {
    let server = Server::with_objects(3, 100);

    let synced = synced(&server, &everything());

    assert_eq!(synced.landed, server.keys());
    for key in server.keys() {
        assert!(synced.path(&key).is_file(), "{key} did not land");
    }
}

/// duckdb reads the objects where they landed, with no unpacking step, under
/// the read the tool prints (`recipe`), and the kind narrows that read to the
/// objects filed under it, which is a claim about real names on disk.
#[test]
fn duckdb_reads_the_downloaded_objects_where_they_landed() {
    let server = Server::with_objects(3, 100);
    let synced = synced(&server, &everything());
    let into = synced.dir.path().join("objects");
    let logs = server
        .keys()
        .iter()
        .filter(|key| key.contains(&Kind::Logs.to_string()))
        .count();

    // One line per object, each carrying its own provenance stamp.
    assert_eq!(rows(&recipe::read(&into, None)), server.keys().len());
    assert_eq!(rows(&recipe::read(&into, Some(Kind::Logs))), logs);
}

/// The download directory is the operator's, and it goes inside a SQL string
/// literal: an unescaped quote in it ends that literal, and duckdb reads a
/// query the tool did not mean.
#[test]
fn duckdb_reads_a_download_directory_whose_name_holds_a_quote() {
    let server = Server::with_objects(1, 100);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("it's objects");
    let state = state_of(dir.path());

    sync::run(
        &server.pulling(),
        &everything().filters(),
        &Destination {
            into: &into,
            state: &state,
        },
        permissive(),
        |_| {},
    )
    .expect("the sync runs");

    assert_eq!(rows(&recipe::read(&into, None)), 1);
}

/// What `read` reads, counted by the duckdb the suite ships with
/// (flake.nix suiteTools).
fn rows(read: &str) -> usize {
    let counted = Command::new("duckdb")
        .args([
            "-noheader",
            "-list",
            "-c",
            &format!("select count(*) from {read}"),
        ])
        .output()
        .expect("duckdb runs");

    assert!(
        counted.status.success(),
        "{read}: {}",
        String::from_utf8_lossy(&counted.stderr)
    );
    String::from_utf8_lossy(&counted.stdout)
        .trim()
        .parse()
        .expect("a count")
}

/// A page bound is the server's, and the walk has to follow it: three objects
/// with one key to a page is three pages plus the empty one after them.
#[test]
fn a_sync_follows_the_listing_past_the_page_bound() {
    let server = Server::with_objects(3, 1);

    let synced = synced(&server, &everything());

    assert_eq!(synced.landed, server.keys());
}

#[test]
fn a_listing_that_does_not_advance_past_the_cursor_is_refused() {
    let key = ObjectName::stamped(
        support::test_now(),
        pool_of(&test_key()),
        AgentId::parse("relay-0").expect("a slug"),
        Kind::Metrics,
    )
    .to_key();
    let archive = support::fixed_listing(&Listing {
        keys: vec![key.clone()],
        truncated: true,
    });
    let mut found = Vec::new();

    let error = sync::list(&archive, &everything().filters(), |key| {
        found.push(key.to_string())
    })
    .expect_err("a listing that repeats its last page is refused");

    assert_eq!(found, vec![key.clone()], "the first page is still a page");
    assert!(
        matches!(&error, SyncError::Stuck { after } if *after == key),
        "got: {error}"
    );
}

#[test]
fn an_empty_page_the_server_calls_truncated_is_not_the_end() {
    let archive = support::fixed_listing(&Listing {
        keys: Vec::new(),
        truncated: true,
    });

    let error = sync::list(&archive, &everything().filters(), |_| {})
        .expect_err("an empty truncated page is refused");

    assert!(
        matches!(&error, SyncError::Stuck { after } if after.is_empty()),
        "got: {error}"
    );
}

/// Resuming is what the cursor is for: the objects a previous run wrote are not
/// listed again, so deleting them cannot bring them back.
#[test]
fn an_interrupted_sync_resumes_after_the_object_it_last_wrote() {
    let server = Server::with_objects(3, 1);
    let keys = server.keys();
    let dir = tempfile::tempdir().expect("a temp dir");
    // A directory where the third object's staging file has to go, so the run
    // stops with two written and the cursor naming the second. A stopped run
    // rather than a killed process, because what resuming reads is the cursor
    // either way.
    let blocked = dir
        .path()
        .join("objects")
        .join(format!("{}.staged", keys[2]));
    std::fs::create_dir_all(&blocked).expect("the blocking directory is created");

    let (error, dir, landed) = sync_into(&server, &everything(), dir)
        .expect_err("a download that cannot be written stops the sync");

    assert_eq!(landed, keys[..2].to_vec(), "stopped early: {error}");
    let cursor = Cursor::read(
        &state_of(dir.path()),
        &everything().filters(),
        &permissive(),
    )
    .expect("the cursor reads");
    assert_eq!(cursor.after, keys[1]);
    // Unblocked, and the two already written deleted: a resumed run must not
    // fetch them, so their absence afterwards is what says it resumed.
    std::fs::remove_dir(&blocked).expect("the blocking directory is removed");
    for key in &keys[..2] {
        std::fs::remove_file(dir.path().join("objects").join(key)).expect("the object was there");
    }

    let resumed =
        sync_into(&server, &everything(), dir).unwrap_or_else(|(error, _, _)| panic!("{error}"));

    assert_eq!(resumed.landed, keys[2..].to_vec());
    for key in &keys[..2] {
        assert!(
            !resumed.path(key).exists(),
            "{key} was downloaded again instead of resumed past"
        );
    }
}

/// A download that stops must leave nothing that reads as the object, and
/// nothing beside it either.
#[test]
fn a_staged_file_does_not_survive_a_download_that_failed() {
    let server = Server::with_objects(1, 100);
    let key = server.keys()[0].clone();
    // Listed and unreadable, so the download fails once the staging file is
    // already open. Permissions, because the failure has to be in the object
    // and not in the walk: a deleted object is not listed at all. The suite
    // never runs as root (`nix build` does not).
    server.unreadable(&key);

    let (error, dir, _) = sync_into(&server, &everything(), tempfile::tempdir().unwrap())
        .expect_err("an unreadable object stops the sync");

    assert!(
        matches!(&*error, SyncError::Pull(pull) if pull.to_string().contains("503")),
        "got: {error}"
    );
    let staged = dir.path().join("objects").join(format!("{key}.staged"));
    assert!(!staged.exists(), "{} was left behind", staged.display());
}

/// The prefix goes upstream, so one day's objects are listed without the server
/// reading a key beyond it.
#[test]
fn a_prefix_selects_a_day_without_downloading() {
    let server = Server::with_objects(3, 100);
    let keys = server.keys();
    let day = keys[1]
        .rsplit_once('/')
        .map(|(folder, _)| format!("{folder}/"))
        .expect("a key names its day folder");

    let (listed, report) = listed(
        &server,
        &Asked {
            prefix: day,
            days: Days::default(),
            selection: Selection::default(),
        },
    );

    assert_eq!(listed, vec![keys[1].clone()]);
    assert_eq!(report.objects, 1);
}

/// The pool, the agent and the kind sit after the id in the key, so no prefix
/// selects them. Read off the key, they still cost no download.
#[test]
fn the_pool_the_agent_and_the_kind_each_select_off_the_key() {
    let server = Server::with_objects(3, 100);
    let keys = server.keys();
    let selections = [
        (
            Selection {
                pool: Some(pool_of(&test_key())),
                ..Selection::default()
            },
            vec![keys[0].clone(), keys[2].clone()],
        ),
        (
            Selection {
                pool: Some(pool_of(&other_key())),
                ..Selection::default()
            },
            vec![keys[1].clone()],
        ),
        (
            Selection {
                agent: Some(AgentId::parse("relay-1").expect("a slug")),
                ..Selection::default()
            },
            vec![keys[1].clone()],
        ),
        (
            Selection {
                kind: Some(Kind::Logs),
                ..Selection::default()
            },
            vec![keys[1].clone()],
        ),
    ];

    for (selection, expected) in selections {
        let named = selection.to_string();
        let (listed, report) = listed(&server, &only(selection));

        assert_eq!(listed, expected, "{named}");
        assert_eq!(report.passed, keys.len() as u64 - expected.len() as u64);
    }
}

/// A filtered sync downloads what it selected and nothing else, and the cursor
/// still passes the keys it left out: they were listed, and re-listing them
/// would download nothing.
#[test]
fn a_filtered_sync_downloads_only_what_it_selected() {
    let server = Server::with_objects(3, 100);
    let keys = server.keys();
    let asked = only(Selection {
        kind: Some(Kind::Logs),
        ..Selection::default()
    });

    let synced = synced(&server, &asked);

    assert_eq!(synced.landed, vec![keys[1].clone()]);
    assert_eq!(synced.report.passed, 2);
    let cursor = Cursor::read(
        &state_of(synced.dir.path()),
        &asked.filters(),
        &permissive(),
    )
    .expect("the cursor reads");
    assert_eq!(cursor.after, keys[2]);
}

/// The cursor passed keys the selection left out, so it only means anything
/// against that selection: read under another one it would skip objects it
/// never downloaded.
#[test]
fn a_cursor_taken_under_other_filters_is_refused() {
    let server = Server::with_objects(3, 100);
    let dir = synced(
        &server,
        &only(Selection {
            kind: Some(Kind::Logs),
            ..Selection::default()
        }),
    )
    .dir;

    let (error, _, landed) =
        sync_into(&server, &everything(), dir).expect_err("another selection is another cursor");

    assert!(landed.is_empty(), "downloaded before refusing: {landed:?}");
    assert!(
        matches!(&*error, SyncError::Cursor(cursor) if cursor.to_string().contains("kind logs")),
        "got: {error}"
    );
}

/// An object this build cannot name is passed over and counted, not taken: the
/// download route refuses such a key, so a sync that asked for it would stall
/// at it every run. The count is what keeps the pass silent about nothing.
#[test]
fn an_object_this_build_cannot_name_is_counted_and_left() {
    let server = Server::with_objects(1, 100);
    let foreign = "v1/2026-08-27/something-else.jsonl.zst";
    server.seed_foreign(foreign, b"not a submission");

    let asked = everything();
    let synced = synced(&server, &asked);

    assert_eq!(synced.landed, server.keys());
    assert_eq!(synced.report.unnameable, 1);
    assert!(!synced.path(foreign).exists());
    // Past it, so the next run does not stop there either.
    let cursor = Cursor::read(
        &state_of(synced.dir.path()),
        &asked.filters(),
        &permissive(),
    )
    .expect("the cursor reads");
    assert_eq!(cursor.after, foreign);
}

/// The credential is what the routes are gated on, and a wrong one has to
/// surface as the refusal it is rather than as an empty sync.
#[test]
fn a_wrong_password_is_reported_not_synced() {
    let server = Server::with_objects(1, 100);
    let asked = everything();

    let error = sync::list(
        &server.pulling_with("developer", "not-the-password"),
        &asked.filters(),
        |_| {},
    )
    .expect_err("a wrong password is refused");

    assert!(
        matches!(&error, SyncError::Pull(pull) if pull.to_string().contains("401")),
        "got: {error}"
    );
}

/// The keys are the server's, so a path that would leave the download
/// directory is refused rather than written.
#[test]
fn a_key_that_is_not_a_relative_path_names_no_file() {
    let into = Path::new("/tmp/downloads");
    for key in ["/etc/passwd", "../escaped", "v1/../../escaped", ""] {
        assert_eq!(sync::destination(into, key), None, "{key:?}");
    }
    assert_eq!(
        sync::destination(into, "v1/2026-08-27/object.jsonl.zst"),
        Some(into.join("v1/2026-08-27/object.jsonl.zst"))
    );
}

/// The scenario `pull::PullError::Oversized` tells an operator to fix: objects
/// over the bound are refused, they raise `--max-object-bytes` as that message
/// says, and re-run. It has to fetch what the lower bound passed over, which
/// means the cursor cannot have been left past it.
#[test]
fn raising_the_size_bound_fetches_what_the_lower_one_refused() {
    let server = Server::attesting(2, 100);
    let keys = server.keys();

    let dir = tempfile::tempdir().expect("a temp dir");
    let refused = sync_verifying(
        &server,
        &everything(),
        dir,
        Verification {
            // Under every object, so this run writes none of them.
            max_object_bytes: NonZeroU64::new(1).unwrap(),
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));
    assert_eq!(refused.report.rejected.len(), 2, "{:?}", refused.report);
    for key in &keys {
        assert!(!refused.path(key).is_file(), "{key} should not have landed");
    }

    // The remedy, on the same state file. The cursor is past objects this
    // bound would have taken, so the run is refused rather than reporting a
    // clean sync of nothing: an operator who raises the flag is told the state
    // file is for another run and to name one of its own.
    let (error, _dir, landed) = sync_verifying(&server, &everything(), refused.dir, permissive())
        .expect_err("a raised bound against the same state file is another run");
    assert!(landed.is_empty(), "{landed:?}");
    let error = error.to_string();
    assert!(
        error.contains("max-object-bytes 1")
            && error.contains(&format!("max-object-bytes {}", 1 << 20)),
        "the refusal must name both bounds, got: {error}"
    );
}

/// And the remedy works: a state file of its own fetches what the lower bound
/// refused. Without this the case above would be satisfied by refusing every
/// second run, which is not a fix.
#[test]
fn a_state_file_of_its_own_fetches_what_the_lower_bound_refused() {
    let server = Server::attesting(2, 100);
    let keys = server.keys();

    let refused = sync_verifying(
        &server,
        &everything(),
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            max_object_bytes: NonZeroU64::new(1).unwrap(),
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("the sync failed: {error}"));
    assert_eq!(refused.report.rejected.len(), 2, "{:?}", refused.report);

    let raised = sync_verifying(
        &server,
        &everything(),
        tempfile::tempdir().expect("a temp dir"),
        permissive(),
    )
    .unwrap_or_else(|(error, _, _)| panic!("the second sync failed: {error}"));

    for key in &keys {
        assert!(
            raised.path(key).is_file(),
            "{key} did not land under a bound that takes it: {:?}",
            raised.report
        );
    }
}

/// A pair that arrived and did not decode ends the run, and neither lands as
/// an object nobody could check nor is stepped over as one bad key.
///
/// Landing it as `unattested` would claim something false about its
/// provenance. Rejecting it per object would be worse: whatever rewrote a
/// header rewrote it for the path, so every key is refused in turn and the
/// cursor ends up past the whole archive.
#[test]
fn a_mangled_attestation_stops_the_run_and_leaves_the_cursor_where_it_was() {
    // A key the shipped server produced, so what this test is about is the
    // headers rather than a name `ObjectName::parse` would pass over.
    let seeded = Server::with_objects(1, 100);
    let key = &seeded.keys()[0];
    let archive = support::stub_archive(
        &[key.as_str()],
        support::Downloads::ManglingTheAttestation(b"whatever the bytes are".to_vec()),
    );
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let state = state_of(dir.path());
    let mut landed = Vec::new();

    let error = sync::run(
        &archive,
        &everything().filters(),
        &Destination {
            into: &into,
            state: &state,
        },
        // Nothing insisted on, which is where an object with no pair does
        // land: this one must not, even here.
        permissive(),
        |key| landed.push(key.to_string()),
    )
    .expect_err("a mangled pair is the path's problem, so it ends the run");

    assert!(landed.is_empty(), "it was written down: {landed:?}");
    assert!(
        matches!(&error, SyncError::Malformed { .. }),
        "got: {error}"
    );
    assert!(error.to_string().contains("did not decode"), "{error}");
    // And the cursor never moved, so fixing whatever mangled the header and
    // running again fetches the object rather than skipping it for good.
    assert!(!state.exists(), "the cursor advanced: {state:?}");
}

/// An object the route refuses outright does not stand in front of the rest of
/// the archive for ever. Asking again next run gets the same 404, so a cursor
/// parked behind it never reaches another object again, and there is no flag
/// to step over one by hand.
#[test]
fn an_object_the_route_refuses_is_reported_and_the_run_goes_on() {
    let seeded = Server::with_objects(1, 100);
    let key = &seeded.keys()[0];
    let archive = support::stub_archive(&[key.as_str()], support::Downloads::Refusing);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let state = state_of(dir.path());

    let report = sync::run(
        &archive,
        &everything().filters(),
        &Destination {
            into: &into,
            state: &state,
        },
        permissive(),
        |_| {},
    )
    .expect("a refused key is one object's problem, not the run's");

    assert_eq!(report.objects, 0, "{report:?}");
    assert_eq!(report.rejected.len(), 1, "{report:?}");
    assert_eq!(&report.rejected[0].key, key);
    assert!(report.rejected[0].reason.contains("404"), "{report:?}");
    // The point of the whole change: the next run starts after it.
    let cursor = Cursor::read(&state, &everything().filters(), &permissive()).expect("it reads");
    assert_eq!(&cursor.after, key);
}

/// And the other half of that split: a run whose archive cannot be reached
/// stops with the cursor where it was, rather than marking everything left
/// rejected and racing to the end of the listing.
#[test]
fn an_archive_that_cannot_be_reached_stops_the_run_where_it_was() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let state = state_of(dir.path());

    let error = sync::run(
        &support::unreachable_archive(),
        &everything().filters(),
        &Destination {
            into: &into,
            state: &state,
        },
        permissive(),
        |_| {},
    )
    .expect_err("an archive that answers nothing is the run's failure");

    assert!(matches!(&error, SyncError::Pull(_)), "got: {error}");
    assert!(!state.exists(), "nothing advanced: {state:?}");
}

/// Two state files may share one `--into`, and that is what left a directory
/// holding proven and assumed objects with nothing to tell them apart: the
/// cursor's bar check is per state file, and a second state file is a second
/// cursor. The directory records its own bar, so the second run is refused.
#[test]
fn a_second_state_file_cannot_fill_one_directory_under_another_bar() {
    let server = Server::attesting(2, 100);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let strict = Verification {
        insist: Insist::ColdSigned,
        ..permissive()
    };
    let sync_with = |state: &Path, verification| {
        sync::run(
            &server.pulling(),
            &everything().filters(),
            &Destination { into: &into, state },
            verification,
            |_| {},
        )
    };

    sync_with(&dir.path().join("cold.json"), strict).expect("the first run fills it");
    let error = sync_with(&dir.path().join("loose.json"), permissive())
        .expect_err("a second bar into one directory is refused");

    assert!(matches!(&error, SyncError::Provenance(_)), "got: {error}");
    // Both bars are named, because the operator has to decide which one the
    // directory is for.
    let said = error.to_string();
    assert!(said.contains("--require-cold-signed"), "{said}");
    assert!(said.contains("no --require flag"), "{said}");
}

/// And the same bar is fine, which is what the usage text offers: two filter
/// sets, two state files, one directory.
#[test]
fn two_state_files_at_one_bar_share_a_directory() {
    let server = Server::attesting(2, 100);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let sync_with = |state: &Path, asked: &Asked| {
        sync::run(
            &server.pulling(),
            &asked.filters(),
            &Destination { into: &into, state },
            permissive(),
            |_| {},
        )
    };

    let first = sync_with(&dir.path().join("one.json"), &everything()).expect("the first run");
    // A second set of filters, so it is a second state file by design rather
    // than a re-run of the first.
    let second = sync_with(
        &dir.path().join("two.json"),
        &only(Selection {
            kind: Some(Kind::Metrics),
            ..Selection::default()
        }),
    )
    .expect("a second state file at the same bar shares the directory");

    assert_eq!(first.cold_signed, 2);
    assert!(second.objects > 0, "{second:?}");
}

/// A 401 is the credential, not the object, and it arrives for every key after
/// the one that met it. Stepping over each in turn would leave the cursor past
/// the whole archive, so the run after the credential is fixed has nothing left
/// to fetch.
#[test]
fn a_refusal_that_is_not_about_the_object_stops_the_run() {
    let seeded = Server::with_objects(1, 100);
    let key = &seeded.keys()[0];
    for status in [400, 401, 403] {
        let archive =
            support::stub_archive(&[key.as_str()], support::Downloads::RefusingWith(status));
        let dir = tempfile::tempdir().expect("a temp dir");
        let into = dir.path().join("objects");
        let state = state_of(dir.path());

        let error = sync::run(
            &archive,
            &everything().filters(),
            &Destination {
                into: &into,
                state: &state,
            },
            permissive(),
            |_| {},
        )
        .expect_err("a status that is not one object's has to end the run");

        assert!(matches!(&error, SyncError::Pull(_)), "{status}: {error}");
        assert!(
            !state.exists(),
            "{status} advanced the cursor past an object it never saw"
        );
    }
}

/// A whole listing of gone objects syncs to completion, naming every one.
///
/// This is the choice, not an accident of counting: an object that is not there
/// is not there for anybody, so the run reports each and moves past it. There
/// is no cap on how many of those a run tolerates, because a cap would stall a
/// bucket whose old objects have been expired and give an operator nothing to
/// do about it.
///
/// What keeps that safe is the classification and nothing else: a status that
/// would be the path's rather than one key's stops the run instead
/// (`a_refusal_that_is_not_about_the_object_stops_the_run`).
#[test]
fn a_listing_of_gone_objects_is_reported_in_full_and_not_stalled() {
    let seeded = Server::with_objects(4, 100);
    let keys = seeded.keys();
    let listed: Vec<&str> = keys.iter().map(String::as_str).collect();
    let archive = support::stub_archive(&listed, support::Downloads::Refusing);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");
    let state = state_of(dir.path());

    let report = sync::run(
        &archive,
        &everything().filters(),
        &Destination {
            into: &into,
            state: &state,
        },
        permissive(),
        |_| {},
    )
    .expect("objects that are gone are the run's to report, not to stop for");

    assert_eq!(report.objects, 0, "{report:?}");
    assert_eq!(report.rejected.len(), keys.len(), "{report:?}");
    // Named, every one: a key nobody can fetch is not a number.
    for key in &keys {
        assert!(
            report.rejected.iter().any(|refused| refused.key == *key),
            "{key} was stepped over and not reported: {report:?}"
        );
    }
    let cursor = Cursor::read(&state, &everything().filters(), &permissive()).expect("it reads");
    assert_eq!(&cursor.after, keys.last().expect("four keys"));
}

/// Every way a pull can fail, with whether it is one key's or the path's.
///
/// A table rather than a spot check, because the cost of getting one wrong is
/// asymmetric: a fault of the path admitted here is reported per object, and
/// every rejection advances the cursor, so the run walks to the end of the
/// archive and the next one has nothing left to ask for. The match in
/// `is_one_objects_problem` has no wildcard, so a new variant fails to compile
/// until it is classified; this is what makes that classification reviewable
/// rather than merely forced.
#[test]
fn every_pull_failure_is_classified_as_one_key_or_the_path() {
    use metsuke_fetch::pull::PullError;

    let url = "https://archive.example.org/v1/object?key=k".to_string();
    let key = "v1/2026-08-27/k.jsonl.zst".to_string();
    let refused = |status| PullError::Refused {
        url: url.clone(),
        status,
        reason: "refused".to_string(),
    };
    // (the failure, whether it is one key's, why)
    let table: Vec<(PullError, bool, &str)> = vec![
        (refused(404), true, "the key is not there to be had"),
        (refused(410), true, "the same, said permanently"),
        (
            PullError::Oversized {
                key: key.clone(),
                length: 1 << 30,
                max: 1 << 24,
            },
            true,
            "this object is over the bound the operator set",
        ),
        (
            refused(400),
            false,
            "the request this build makes, so every object's",
        ),
        (refused(401), false, "the credential, so every object's"),
        (
            refused(403),
            false,
            "the same, and a WAF answers it per route",
        ),
        (
            refused(429),
            false,
            "the window rolls, so asking again differs",
        ),
        (
            refused(500),
            false,
            "the server, and it may answer differently",
        ),
        (refused(503), false, "the same"),
        (
            PullError::NoLength { key: key.clone() },
            false,
            "how a proxy that re-frames answers looks, so every object's",
        ),
        (
            PullError::Short {
                key: key.clone(),
                read: 10,
                length: 20,
            },
            false,
            "a flaky link, and the next run may read it whole",
        ),
        (
            PullError::Unread {
                key: key.clone(),
                source: std::io::Error::from(std::io::ErrorKind::ConnectionReset),
            },
            false,
            "the same",
        ),
        (
            PullError::Unreachable {
                url: url.clone(),
                reason: "connection refused".to_string(),
            },
            false,
            "the archive, not an object in it",
        ),
        (
            PullError::UnreadableListing {
                url: url.clone(),
                reason: "not json".to_string(),
            },
            false,
            "the listing route, so no object was ever named",
        ),
    ];

    for (failure, one_key, why) in &table {
        assert_eq!(
            failure.is_one_objects_problem(),
            *one_key,
            "{failure} should be {}: {why}",
            match one_key {
                true => "one key's, reported and stepped over",
                false => "the path's, ending the run",
            }
        );
    }
    // The statuses that may be stepped over, stated once here and once in the
    // source, so widening one without the other fails. Sorted, because the
    // assertion is about the set and not about where a row sits in the table.
    let mut steppable = table
        .iter()
        .filter_map(|(failure, _, _)| match failure {
            PullError::Refused { status, .. } if failure.is_one_objects_problem() => Some(*status),
            _ => None,
        })
        .collect::<Vec<u16>>();
    steppable.sort_unstable();
    assert_eq!(steppable, vec![404, 410]);
}

/// A `--require-*` flag filters, so a run of consecutive objects below the bar
/// is ordinary and syncs to completion.
///
/// Objects are keyed by uuidv7 and so arrive in order, and a pool signing with
/// its Leios key produces long unbroken runs of them, which is the shape the
/// pipe setup asks for. Anything that gave up after a few refusals in a row
/// would make `--require-cold-signed` unusable against exactly the archive
/// this program is built for.
#[test]
fn require_cold_signed_filters_a_whole_run_of_leios_objects() {
    let server = Server::attesting(4, 100);
    let keys = server.keys();
    for key in &keys {
        server.leios_sign(key);
    }

    let synced = sync_verifying(
        &server,
        &everything(),
        tempfile::tempdir().expect("a temp dir"),
        Verification {
            insist: Insist::ColdSigned,
            ..permissive()
        },
    )
    .unwrap_or_else(|(error, _, _)| panic!("filtering is not a failure: {error}"));

    assert_eq!(synced.report.objects, 0, "{:?}", synced.report);
    assert_eq!(
        synced.report.rejected.len(),
        keys.len(),
        "{:?}",
        synced.report
    );
    // And the cursor is past all of them, so the next run does not re-fetch
    // objects this bar will refuse again.
    let cursor = Cursor::read(
        &state_of(synced.dir.path()),
        &everything().filters(),
        &Verification {
            insist: Insist::ColdSigned,
            ..permissive()
        },
    )
    .expect("the cursor reads");
    assert_eq!(&cursor.after, keys.last().expect("four keys"));
}
