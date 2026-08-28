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
