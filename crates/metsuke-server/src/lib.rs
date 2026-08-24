//! Ingest server: everything between a received upload and an archived
//! object. `intake` is the single entry point; the other modules are
//! adapters it drives.

pub mod applications;
pub mod archive;
pub mod authority;
pub mod calidus;
pub mod cli;
pub mod config;
pub mod counters;
pub mod http;
pub mod intake;
pub mod ratelimit;
pub mod rebuild;
pub mod s3;
pub mod verify;

/// The agent version this server nudges operators towards (`build.rs`).
pub const CLIENT_VERSION: &str = env!("CLIENT_VERSION");
