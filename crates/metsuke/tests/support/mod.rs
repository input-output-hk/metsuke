//! Helpers shared by the integration tests that speak the upload wire
//! format; a `tests/support` module so cargo doesn't build it as its own
//! test target.
//!
//! Every test target compiles the whole module, so a helper only one of them
//! needs reads as dead code in the others.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use metsuke::config::{Config, LogConfig};
use metsuke::logselect::SelectConfig;
use metsuke_wire::envelope::{Limits, SigningKey};

/// The all-sevens test seed used across the suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// Wide enough for any test batch; the real limits are server config.
pub const TEST_LIMITS: Limits = Limits {
    max_header_bytes: 4096,
    max_decompressed_bytes: 64 * 1024 * 1024,
};

/// A journalctl stand-in: a program that answers whatever arguments it is given
/// with `recording`, so what a test exercises is the reading and not the flags.
/// A two-line script is enough, because journalctl's own output is what a
/// recording already is (tests/fixtures/README.md).
pub fn replaying_journalctl(dir: &tempfile::TempDir, recording: &Path) -> PathBuf {
    let path = dir.path().join("journalctl");
    std::fs::write(
        &path,
        format!("#!/bin/sh\nexec cat {}\n", recording.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// The `[log]` section an operator gets without writing any of it: the shipped
/// defaults, read out of the config that ships them rather than restated here.
pub fn shipped_log_config() -> LogConfig {
    Config::from_toml(
        r#"
        pool_id = "pool1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq8a7a2d"
        metrics_url = "http://127.0.0.1:12798/metrics"
        upload_url = "https://metsuke.example.org/v1/submit"
        [log]
        journal_unit = "cardano-node"
        journalctl_path = "/usr/bin/journalctl"
        "#,
    )
    .unwrap()
    .log
    .unwrap()
}

/// The selection rules those defaults make. That the shipped namespaces pass
/// the shipped roots is part of what this asserts: `new` is the only way to
/// hold rules at all.
pub fn shipped_rules() -> SelectConfig {
    let log = shipped_log_config();
    SelectConfig::new(&log.namespace_roots, log.namespaces, log.min_severity).unwrap()
}

/// One of the recorded node streams under tests/fixtures/recordings.
pub fn recording(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recordings")
        .join(name)
}
