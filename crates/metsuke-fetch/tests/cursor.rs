//! The state file: what a resumed run reads, and what it refuses to read.

use metsuke_fetch::cursor::{Cursor, CursorError};
use metsuke_fetch::select::{Filters, Selection};
use metsuke_wire::key::{KEY_PREFIX, Kind};

/// A run's filters, owned so a test can point two reads at them.
struct Asked {
    prefix: String,
    selection: Selection,
}

impl Asked {
    fn filters(&self) -> Filters<'_> {
        Filters {
            prefix: &self.prefix,
            selection: &self.selection,
        }
    }
}

fn asked(prefix: &str, selection: Selection) -> Asked {
    Asked {
        prefix: prefix.to_string(),
        selection,
    }
}

fn everything() -> Asked {
    asked(KEY_PREFIX, Selection::default())
}

#[test]
fn a_state_file_that_does_not_exist_yet_is_the_archives_start() {
    let dir = tempfile::tempdir().expect("a temp dir");

    let cursor = Cursor::read(&dir.path().join("cursor.json"), &everything().filters())
        .expect("an absent file reads");

    assert_eq!(cursor.after, "");
    assert_eq!(cursor.prefix, KEY_PREFIX);
    assert_eq!(cursor.selection, Selection::default());
}

#[test]
fn an_advanced_cursor_is_what_the_next_run_reads() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("state").join("cursor.json");
    let asked = everything();
    let mut cursor = Cursor::read(&path, &asked.filters()).expect("an absent file reads");

    cursor
        .advance(&path, "v1/2026-08-27/object.jsonl.zst")
        .expect("the state file writes");

    assert_eq!(
        Cursor::read(&path, &asked.filters()).expect("it reads back"),
        cursor
    );
}

/// Both halves of the filters are checked, because a run advances the cursor
/// past every key it saw under them.
#[test]
fn a_cursor_from_other_filters_is_refused() {
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
    for (index, (asking, wanted)) in [
        (
            everything(),
            "prefix \"v1/\" and every pool, agent and kind",
        ),
        (
            asked("v1/2026-08-01/", Selection::default()),
            "prefix \"v1/2026-08-01/\" and every pool, agent and kind",
        ),
        (
            asked(KEY_PREFIX, held.selection.clone()),
            "prefix \"v1/\" and kind metrics",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let path = dir.path().join(format!("cursor-{index}.json"));
        Cursor::read(&path, &held.filters())
            .expect("an absent file reads")
            .advance(&path, "v1/2026-08-01/object.jsonl.zst")
            .expect("the state file writes");

        let error =
            Cursor::read(&path, &asking.filters()).expect_err("other filters are another cursor");

        assert!(
            matches!(&error, CursorError::OtherFilters { held, asked, .. }
                if held == "prefix \"v1/2026-08-01/\" and kind metrics" && asked == wanted),
            "got: {error}"
        );
    }
}

#[test]
fn a_state_file_that_is_not_a_cursor_is_refused() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("cursor.json");
    std::fs::write(&path, "v1/2026-08-27/object.jsonl.zst\n").expect("the file writes");

    let error =
        Cursor::read(&path, &everything().filters()).expect_err("a bare key is not the state file");

    assert!(
        matches!(&error, CursorError::Unreadable { .. }),
        "got: {error}"
    );
}
