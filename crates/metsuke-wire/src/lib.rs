//! What both binaries need: the wire contract they agree on, plus the
//! journald prefixes, hex and schema migration each would otherwise keep its
//! own copy of. Living here means the server verifies uploads without
//! linking the agent's sampling, spooling and HTTP client code, so an agent
//! dependency never becomes a server dependency.

pub mod envelope;
pub mod hex;
pub mod journal;
pub mod sqlite;
