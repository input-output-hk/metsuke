//! Ingest server: everything between a received upload and an archived
//! object. `intake` is the single entry point; the other modules are
//! adapters it drives.

pub mod archive;
pub mod config;
pub mod counters;
pub mod intake;
pub mod ratelimit;
