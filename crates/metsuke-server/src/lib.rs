//! Ingest server: everything between a received upload and an archived
//! object. `intake` is the single entry point; the other modules are
//! adapters it drives. Nothing here holds state across a restart. The
//! bucket is the only store.

pub mod applications;
pub mod archive;
pub mod authority;
pub mod cli;
pub mod config;
pub mod developer;
pub mod http;
pub mod instructions;
pub mod intake;
pub mod ratelimit;
pub mod roster;
pub mod s3;
pub mod serve;
pub mod verify;

/// The agent version this server nudges operators towards (`build.rs`).
pub const CLIENT_VERSION: &str = env!("CLIENT_VERSION");
