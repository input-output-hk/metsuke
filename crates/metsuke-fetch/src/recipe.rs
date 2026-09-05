//! How a downloaded archive is read back, as one duckdb table function, so a
//! sync can print the read for the directory it just wrote.
//! docs/reading-the-archive.md is the same read for a consumer who has not run
//! one.

use std::path::Path;

use metsuke_wire::key::{KEY_PREFIX, KEY_SUFFIX, Kind};

/// The objects `into` holds, or only those of one `kind`. What the arguments
/// are for is docs/reading-the-archive.md.
///
/// `into` is escaped into the SQL literal and no further: a download directory
/// whose own name holds `*`, `?` or `[` is one this read cannot name, because
/// those are the glob's.
///
/// Bracketing them is not the fix and was measured not to be. `*` and `[`
/// survive it, and on duckdb 1.5.5 a directory named `q?m` read back through
/// `q[?]m` handed the compressed bytes to the JSON parser, which is a wrong
/// answer rather than an error. So the escape stops at the quote, and
/// docs/reading-the-archive.md tells a consumer to name the directory without
/// them.
pub fn read(into: &Path, kind: Option<Kind>) -> String {
    let file = match kind {
        Some(kind) => format!("*-{kind}{KEY_SUFFIX}"),
        None => format!("*{KEY_SUFFIX}"),
    };
    // A day folder per key, so one `*` under the schema prefix reaches every
    // object without a recursive walk.
    let glob = into.join(format!("{KEY_PREFIX}*/{file}"));
    format!(
        "read_json('{}', sample_size=-1)",
        glob.display().to_string().replace('\'', "''")
    )
}
