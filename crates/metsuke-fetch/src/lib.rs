//! The archive as local files: list a prefix, download what is new, remember
//! where the download got to. A separate binary from the server and the agent
//! because it is a developer's tool on a developer's machine — it holds the
//! pull credential and touches neither a signing key nor a bucket.
//!
//! Objects are written exactly as the archive holds them, so what lands on
//! disk is still the bytes a pool signed and is read by duckdb as it stands.

pub mod cli;
pub mod cursor;
pub mod pull;
pub mod select;
mod staged;
pub mod sync;
