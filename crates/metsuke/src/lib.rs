//! The SPO agent: scrape cardano-node, spool, seal, upload. The wire
//! contract it speaks lives in `metsuke-wire`.

/// This agent build's version, as it rides in every envelope.
pub const AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod agent;
pub mod cli;
pub mod config;
pub mod delivery;
pub mod endpoint;
pub mod identity;
pub mod keys;
pub mod logselect;
pub mod logsource;
pub mod logtail;
pub mod report;
pub mod schedule;
pub mod scrape;
pub mod scraper;
pub mod sntp;
pub mod spool;
pub mod uploader;
