//! Wire contract shared by the metsuke agent and metsuke-server. The server
//! depends on this crate as a library so both sides trust one definition.

/// This agent build's version, and what the server links this crate for
/// (see `envelope::Ack`).
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent;
pub mod cli;
pub mod config;
pub mod delivery;
pub mod envelope;
pub mod keys;
pub mod sampler;
pub mod schedule;
pub mod scrape;
pub mod sntp;
pub mod spool;
pub mod sqlite;
pub mod uploader;
