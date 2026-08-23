//! sd-daemon severity prefixes journald parses off stderr. Both binaries
//! log this way, and a prefix that drifts between them is a severity that
//! silently lies in the journal.

pub const ERR: &str = "<3>";
pub const WARNING: &str = "<4>";
pub const INFO: &str = "<6>";
