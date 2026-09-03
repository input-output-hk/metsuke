//! What both binaries need: the wire contract they agree on, plus the
//! journald prefixes, hex, schema migration and HTTP client each would
//! otherwise keep its own copy of. Living here means the server verifies
//! uploads without linking the agent's scraping and spooling, so an agent
//! dependency never becomes a server dependency.

/// The commit the binaries were built from, or `unknown` where neither the
/// flake nor a repository said (`build.rs`). Here rather than in each binary
/// because all three link this crate and none of them build differently.
pub const BUILD_REV: &str = env!("BUILD_REV");

/// How a binary names itself, in `--version` and on the line it starts with.
/// The version is the crate's own, so this takes it rather than holding one.
pub fn version_line(version: &str) -> String {
    format!("{version} ({BUILD_REV})")
}

pub mod envelope;
#[cfg(feature = "fixtures")]
pub mod fixtures;
pub mod hex;
pub mod http;
pub mod journal;
pub mod key;
pub mod leios;
pub mod sqlite;
