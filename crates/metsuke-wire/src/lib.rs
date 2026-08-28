//! What both binaries need: the wire contract they agree on, plus the
//! journald prefixes, hex, schema migration and HTTP client each would
//! otherwise keep its own copy of. Living here means the server verifies
//! uploads without linking the agent's scraping and spooling, so an agent
//! dependency never becomes a server dependency.

pub mod envelope;
#[cfg(feature = "fixtures")]
pub mod fixtures;
pub mod hex;
pub mod http;
pub mod journal;
pub mod key;
pub mod sqlite;
