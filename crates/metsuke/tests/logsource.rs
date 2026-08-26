//! The transport, against a journalctl stand-in replaying a recorded stream.
//! What is recorded is the node's stdout (tests/fixtures/README.md).

use metsuke::logsource::{JournalConfig, JournalSource, LineSource, LineSourceError};

mod support;
use support::{recording, replaying_journalctl};

const STARTUP_RECORDING: &str = "leios-node-traces-startup.log";
const STARTUP_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces-startup.log");

#[test]
fn every_line_arrives_in_order_and_the_stream_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = JournalSource::spawn(&JournalConfig {
        journal_unit: "cardano-node".to_string(),
        journalctl_path: replaying_journalctl(&dir, &recording(STARTUP_RECORDING)),
    })
    .unwrap();

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, STARTUP_WINDOW.lines().collect::<Vec<_>>());
    // The end is an end, not an error: the caller's respawn decision hangs on
    // telling the two apart.
    assert_eq!(source.next_line().unwrap(), None);
}

// A journalctl that is not where the config says fails at startup rather than
// leaving a thread quietly reading nothing.
#[test]
fn a_journalctl_that_is_not_there_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let spawned = JournalSource::spawn(&JournalConfig {
        journal_unit: "cardano-node".to_string(),
        journalctl_path: dir.path().join("no-such-journalctl"),
    });
    let Err(error) = spawned else {
        panic!("spawning a journalctl that is not there has to fail");
    };
    assert!(
        matches!(error, LineSourceError::Spawn { .. }),
        "expected a spawn failure naming the path, got: {error}"
    );
    assert!(error.to_string().contains("no-such-journalctl"), "{error}");
}
