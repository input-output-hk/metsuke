//! Both transports: the journal against a journalctl stand-in replaying a
//! recorded stream, and the pipe against a reader and a writer standing in for
//! the node's stdout and whatever consumes it after metsuke. What is recorded
//! is the node's stdout (tests/fixtures/README.md).

use std::io::Write;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use metsuke::config::{Config, LogSource};
use metsuke::logsource::{
    JournalConfig, JournalSource, LineSource, LineSourceError, PipeConfig, PipeSource,
};

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

/// What a downstream consumer of the node's output would see, so a test can
/// compare it with what went in.
#[derive(Clone, Default)]
struct Downstream(Arc<Mutex<Vec<u8>>>);

impl Downstream {
    fn written(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for Downstream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A consumer that has closed the pipe. The write fails; the node must not.
struct Closed;

impl Write for Closed {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }
}

fn queue_of(capacity: usize) -> PipeConfig {
    PipeConfig {
        queue_capacity: NonZeroUsize::new(capacity).unwrap(),
    }
}

/// Wait for the tee thread to have done something observable, so a test asserts
/// on a finished tee rather than on a race.
fn wait_until(condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the tee never got there");
}

/// Three lines and a fourth the node had not finished writing when it exited.
const NODE_OUTPUT: &str = "first\nsecond\nthird\npartial";

#[test]
fn every_line_is_teed_through_byte_for_byte_and_then_offered() {
    let downstream = Downstream::default();
    let mut source = PipeSource::tee(
        std::io::Cursor::new(NODE_OUTPUT),
        downstream.clone(),
        &queue_of(64),
    );

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, ["first", "second", "third", "partial"]);
    // Byte for byte, including the last line's missing terminator: the tee adds
    // and removes nothing from what the node wrote.
    assert_eq!(downstream.written(), NODE_OUTPUT);
    assert_eq!(source.dropped(), 0);
    // EOF on stdin is the node having exited, not a failure to respawn through.
    assert_eq!(source.next_line().unwrap(), None);
}

// The rule the block producer's life hangs on: a queue nobody is draining
// still gets every line written through, and costs the node nothing.
#[test]
fn a_full_queue_drops_lines_rather_than_make_the_node_wait() {
    let downstream = Downstream::default();
    let source = PipeSource::tee(
        std::io::Cursor::new(NODE_OUTPUT),
        downstream.clone(),
        &queue_of(1),
    );

    wait_until(|| downstream.written() == NODE_OUTPUT);
    wait_until(|| source.dropped() == 3);
}

// A downstream that closed must cost the node nothing either: writing fails,
// stdin keeps being drained, and collection carries on.
#[test]
fn a_closed_downstream_does_not_stop_the_drain() {
    let mut source = PipeSource::tee(std::io::Cursor::new(NODE_OUTPUT), Closed, &queue_of(64));

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, ["first", "second", "third", "partial"]);
}

/// A stdin that hands over one line and then fails, which is not the node
/// closing its output.
struct FailsAfterOneLine(bool);

impl std::io::Read for FailsAfterOneLine {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("BufRead::read_line is what the tee calls")
    }
}

impl std::io::BufRead for FailsAfterOneLine {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        match self.0 {
            false => {
                self.0 = true;
                Ok(b"first\n")
            }
            true => Err(std::io::Error::other("the pipe broke")),
        }
    }

    fn consume(&mut self, _: usize) {}
}

#[test]
fn a_read_that_fails_is_not_the_node_exiting() {
    let mut source = PipeSource::tee(FailsAfterOneLine(false), Vec::new(), &queue_of(64));

    assert_eq!(source.next_line().unwrap(), Some("first".to_string()));
    let error = source
        .next_line()
        .expect_err("a failed read is not end of stream");

    assert!(
        error.to_string().contains("the pipe broke"),
        "the failure names what it was: {error}"
    );
}

fn log_section(section: &str) -> Result<LogSource, String> {
    let toml = format!(
        r#"
        pool_id = "pool1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq8a7a2d"
        metrics_url = "http://127.0.0.1:12798/metrics"
        upload_url = "https://metsuke.example.org/v1/submit"
        [log]
        {section}
        "#
    );
    Config::from_toml(&toml)
        .map(|config| config.log.expect("the section is there").source)
        .map_err(|error| error.to_string())
}

#[test]
fn the_pipe_is_chosen_in_the_config_and_needs_nothing_about_the_journal() {
    assert_eq!(
        log_section("source = \"pipe\"").unwrap(),
        LogSource::Pipe(queue_of(4096)),
    );
    assert_eq!(
        log_section("source = \"pipe\"\npipe_queue_capacity = 8").unwrap(),
        LogSource::Pipe(queue_of(8)),
    );
}

// A section that names both sources is an operator who meant one of them.
#[test]
fn a_journal_key_under_the_pipe_fails_loudly() {
    let error = log_section("source = \"pipe\"\njournal_unit = \"cardano-node\"").unwrap_err();
    assert!(error.contains("journal_unit"), "{error}");
    let error = log_section(
        "source = \"journald\"\njournal_unit = \"cardano-node\"\n\
         journalctl_path = \"/usr/bin/journalctl\"\npipe_queue_capacity = 8",
    )
    .unwrap_err();
    assert!(error.contains("pipe_queue_capacity"), "{error}");
}

// A queue of nothing would drop every line, so the config refuses it rather
// than collecting nothing quietly.
#[test]
fn a_queue_capacity_of_zero_fails_loudly() {
    let error = log_section("source = \"pipe\"\npipe_queue_capacity = 0").unwrap_err();
    assert!(error.contains("pipe_queue_capacity"), "{error}");
}
