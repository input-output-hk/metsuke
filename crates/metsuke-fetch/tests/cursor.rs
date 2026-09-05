//! The state file: what a resumed run reads, and what it refuses to read.

use metsuke_fetch::cursor::{Cursor, CursorError};
use metsuke_fetch::select::{Days, Filters, Selection};
use metsuke_fetch::sync::Insist;
use metsuke_wire::key::{KEY_PREFIX, Kind};

/// A run's filters, owned so a test can point two reads at them.
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

fn asked(prefix: &str, selection: Selection) -> Asked {
    Asked {
        prefix: prefix.to_string(),
        days: Days::default(),
        selection,
    }
}

fn everything() -> Asked {
    asked(KEY_PREFIX, Selection::default())
}

#[test]
fn a_state_file_that_does_not_exist_yet_is_the_archives_start() {
    let dir = tempfile::tempdir().expect("a temp dir");

    let cursor = Cursor::read(
        &dir.path().join("cursor.json"),
        &everything().filters(),
        Insist::Nothing,
    )
    .expect("an absent file reads");

    assert_eq!(cursor.after, "");
    assert_eq!(cursor.prefix, KEY_PREFIX);
    assert_eq!(cursor.selection, Selection::default());
    assert_eq!(cursor.insist, Insist::Nothing);
}

/// A state file written before the bar was recorded reads as the lowest one,
/// so the first run that asks for more is refused rather than resuming past
/// objects that run would have wanted.
#[test]
fn a_state_file_without_a_bar_reads_as_the_lowest() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("cursor.json");
    std::fs::write(
        &path,
        r#"{"prefix":"v1/","selection":{"pool":null,"agent":null,"kind":null},"after":"v1/x"}"#,
    )
    .expect("the file writes");

    let cursor = Cursor::read(&path, &everything().filters(), Insist::Nothing)
        .expect("the lowest bar matches");
    assert_eq!(cursor.insist, Insist::Nothing);
    assert_eq!(cursor.after, "v1/x");

    let error = Cursor::read(&path, &everything().filters(), Insist::Attested)
        .expect_err("a higher bar is another run");
    assert!(
        matches!(&error, CursorError::OtherFilters { asked, .. }
            if asked.ends_with("--require-attested")),
        "got: {error}"
    );
}

#[test]
fn an_advanced_cursor_is_what_the_next_run_reads() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("state").join("cursor.json");
    let asked = everything();
    let mut cursor =
        Cursor::read(&path, &asked.filters(), Insist::Nothing).expect("an absent file reads");

    cursor
        .advance(&path, "v1/2026-08-27/object.jsonl.zst")
        .expect("the state file writes");

    assert_eq!(
        Cursor::read(&path, &asked.filters(), Insist::Nothing).expect("it reads back"),
        cursor
    );
}

/// Every part of what a run was asked for is checked, because the run advanced
/// the cursor past every key it saw under all of them. One case per part, each
/// differing in that part alone.
#[test]
fn a_cursor_from_another_run_is_refused() {
    const HELD: &str = "prefix \"v1/2026-08-01/\", kind metrics, --require-cold-signed";
    let dir = tempfile::tempdir().expect("a temp dir");
    let held = asked(
        "v1/2026-08-01/",
        Selection {
            kind: Some(Kind::Metrics),
            ..Selection::default()
        },
    );
    // Each case names what the refusal has to say it was asked for, because
    // that half is the reason the variant carries two strings.
    for (index, (asking, insist, wanted)) in [
        (
            everything(),
            Insist::ColdSigned,
            "prefix \"v1/\", every pool, agent and kind, --require-cold-signed",
        ),
        (
            asked("v1/2026-08-01/", Selection::default()),
            Insist::ColdSigned,
            "prefix \"v1/2026-08-01/\", every pool, agent and kind, --require-cold-signed",
        ),
        (
            asked(KEY_PREFIX, held.selection.clone()),
            Insist::ColdSigned,
            "prefix \"v1/\", kind metrics, --require-cold-signed",
        ),
        // The filters agree and the bar does not. The run that wrote this
        // cursor refused what it would not write and advanced past it anyway,
        // so resuming under a lower bar starts after objects it now wants.
        (
            asked("v1/2026-08-01/", held.selection.clone()),
            Insist::Attested,
            "prefix \"v1/2026-08-01/\", kind metrics, --require-attested",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("cursor-{index}.json"));
        Cursor::read(&path, &held.filters(), Insist::ColdSigned)
            .expect("an absent file reads")
            .advance(&path, "v1/2026-08-01/object.jsonl.zst")
            .expect("the state file writes");

        let error = Cursor::read(&path, &asking.filters(), insist)
            .expect_err("another run is another cursor");

        assert!(
            matches!(&error, CursorError::OtherFilters { held, asked, .. }
                if held == HELD && asked == wanted),
            "got: {error}"
        );
        // The two sit on their own lines at one indent, so the reader finds
        // the part that differs rather than diffing two long strings.
        let text = error.to_string();
        assert!(
            text.contains(&format!("\n  holds: {HELD}\n"))
                && text.contains(&format!("\n  asked: {wanted}\n")),
            "got: {text}"
        );
    }
}

/// The asymmetry between the two bounds. A first day relocates where the
/// listing starts, so a cursor taken with one and read without would resume
/// past everything before it. A last day only stops the walk, in the same
/// direction the cursor moves, so nothing is ever passed over by it and a run
/// that drops it is the natural way to carry on.
#[test]
fn the_first_day_binds_a_cursor_and_the_last_day_does_not() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let bounded = Asked {
        prefix: KEY_PREFIX.to_string(),
        selection: Selection::default(),
        days: Days {
            from: Some("v1/2026-09-01".to_string()),
            until: Some("v1/2026-09-04".to_string()),
        },
    };

    let path = dir.path().join("cursor.json");
    Cursor::read(&path, &bounded.filters(), Insist::Nothing)
        .expect("an absent file reads")
        .advance(&path, "v1/2026-09-02/object.jsonl.zst")
        .expect("the state file writes");

    // Dropping the last day carries on from where the bounded run stopped.
    let carried = Asked {
        days: Days {
            until: None,
            ..bounded.days.clone()
        },
        ..everything()
    };
    let cursor = Cursor::read(&path, &carried.filters(), Insist::Nothing)
        .expect("the last day is not part of what a cursor is for");
    assert_eq!(cursor.after, "v1/2026-09-02/object.jsonl.zst");

    // Dropping the first day is another run, and would resume past August.
    let widened = Asked {
        days: Days::default(),
        ..everything()
    };
    let error = Cursor::read(&path, &widened.filters(), Insist::Nothing)
        .expect_err("no first day is a wider window");
    assert!(
        matches!(&error, CursorError::OtherFilters { held, .. }
            if held.contains("from \"v1/2026-09-01\"")),
        "got: {error}"
    );
}

#[test]
fn a_state_file_that_is_not_a_cursor_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("cursor.json");
    std::fs::write(&path, "v1/2026-08-27/object.jsonl.zst\n").expect("the file writes");

    let error = Cursor::read(&path, &everything().filters(), Insist::Nothing)
        .expect_err("a bare key is not the state file");

    assert!(
        matches!(&error, CursorError::Unreadable { .. }),
        "got: {error}"
    );
}
