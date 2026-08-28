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
use metsuke::logsource::{JournalConfig, JournalSource, Spawned, StartError};
use metsuke_wire::envelope::{AgentId, Limits, PoolId, Provenance, Scrape, SigningKey, TraceLine};
use metsuke_wire::fixtures;
use time::OffsetDateTime;

/// The all-sevens test seed used across the suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// The machine name every batch in the suite is stamped with.
pub fn test_agent_id() -> AgentId {
    AgentId::parse("test-relay").expect("a fixed name is a slug")
}

/// The pool `test_key` speaks for (`identity::check_pool_id` refuses any other).
pub fn test_pool_id() -> PoolId {
    PoolId::from_cold_key(&test_key().verifying_key())
}

/// What every row the suite spools is stamped with, and therefore what a batch
/// drawn from that spool names in its header.
pub fn test_provenance() -> Provenance {
    Provenance {
        pool_id: test_pool_id(),
        agent_id: test_agent_id(),
    }
}

/// A shared fixture scrape whose metric value is its own timestamp, so a row
/// spooled here is distinguishable from every other by either half.
pub fn scrape_at(unix_secs: i64) -> Scrape {
    fixtures::block_number_scrape(
        OffsetDateTime::from_unix_timestamp(unix_secs).expect("a fixed instant"),
        u64::try_from(unix_secs).expect("the suite's instants are past the epoch"),
    )
}

/// The metric the recorded bodies are read by across the suite, as an integer.
/// `None` when the scrape states no such metric — a failed scrape, or a body
/// from before the node emitted it.
pub fn block_number(scrape: &Scrape) -> Option<u64> {
    scrape
        .metrics
        .iter()
        .find(|metric| metric.name == fixtures::BLOCK_NUMBER)?
        .value
        .as_u64()
}

/// A trace line as the spool and the wire hold one: the node's object, parsed.
pub fn trace_line(line: &str) -> TraceLine {
    TraceLine::parse(line).unwrap_or_else(|error| panic!("{line:.60}: {error}"))
}

/// Wide enough for any test batch; the real limits are server config.
pub const TEST_LIMITS: Limits = Limits {
    max_header_bytes: 4096,
    max_decompressed_bytes: 64 * 1024 * 1024,
};

/// An executable `/bin/sh` script at `name`, for the tests that need a program
/// where a configured binary is expected.
pub fn sh_stand_in(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// A journalctl stand-in: a program that answers whatever arguments it is given
/// with `recording`, so what a test exercises is the reading and not the flags.
/// One line is enough, because journalctl's own output is what a recording
/// already is (tests/fixtures/README.md).
pub fn replaying_journalctl(dir: &tempfile::TempDir, recording: &Path) -> PathBuf {
    sh_stand_in(
        dir,
        "journalctl",
        &format!("exec cat {}", recording.display()),
    )
}

/// Spawn a stand-in as if it were the node's journalctl.
///
/// Retries `ETXTBSY`, which is the harness racing with itself rather than
/// anything about the source: these tests write an executable while their
/// siblings fork, and a forked child holds that file's write fd until it execs,
/// so a spawn inside that window is refused. Every stand-in this process spawns
/// goes through here, so the retry is stated once; one the agent binary spawns
/// for itself does not.
pub fn spawning(journalctl_path: PathBuf) -> Spawned {
    for _ in 0..RETRIES_ON_BUSY {
        match JournalSource::spawn(&JournalConfig {
            journal_unit: TEST_UNIT.to_string(),
            journalctl_path: journalctl_path.clone(),
            start_grace: TEST_START_GRACE,
        }) {
            Ok(spawned) => return spawned,
            Err(error) if is_text_file_busy(&error) => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => panic!("{}: {error}", journalctl_path.display()),
        }
    }
    panic!("{} stayed busy", journalctl_path.display());
}

/// The stream without the start check (`Spawned::unconfirmed`).
pub fn replaying(journalctl_path: PathBuf) -> JournalSource {
    spawning(journalctl_path).unconfirmed()
}

/// Long enough for a stand-in that exits at once to have exited. Only a test
/// that confirms a start pays it.
pub const TEST_START_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

/// The unit every test in the suite follows. Which unit it is decides nothing:
/// the stand-in answers whatever arguments it is given.
pub const TEST_UNIT: &str = "cardano-node";

const RETRIES_ON_BUSY: u32 = 50;

fn is_text_file_busy(error: &StartError) -> bool {
    match error {
        StartError::Spawn { source, .. } => source.kind() == std::io::ErrorKind::ExecutableFileBusy,
        StartError::NotFollowing { .. } => false,
    }
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
        source = "journald"
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
    SelectConfig::new(&log.namespace_roots, log.namespaces).unwrap()
}

/// One of the recorded node streams under tests/fixtures/recordings.
pub fn recording(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/recordings")
        .join(name)
}
