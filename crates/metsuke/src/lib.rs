//! Wire contract shared by the metsuke agent and metsuke-server. The server
//! depends on this crate as a library so both sides trust one definition.

pub mod envelope;
pub mod sampler;
pub mod scrape;
pub mod sntp;
