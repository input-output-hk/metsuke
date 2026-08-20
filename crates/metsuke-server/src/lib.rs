//! Ingest server: everything between a received upload and an archived
//! object. `intake` is the single entry point; the other modules are
//! adapters it drives.

pub mod archive;
pub mod cli;
pub mod config;
pub mod counters;
pub mod http;
pub mod intake;
pub mod ratelimit;
pub mod rebuild;
pub mod s3;
pub mod verify;

/// sd-daemon severity prefixes journald parses off stderr.
pub const ERR: &str = "<3>";
pub const WARNING: &str = "<4>";
pub const INFO: &str = "<6>";
