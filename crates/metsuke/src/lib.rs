//! Wire contract shared by the metsuke agent and metsuke-server. The server
//! depends on this crate as a library so both sides trust one definition.

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
pub mod uploader;
