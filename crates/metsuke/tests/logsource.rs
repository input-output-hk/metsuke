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
    JournalConfig, JournalSource, LineSource, PipeConfig, PipeSource, StartError,
};

mod support;
use support::{
    TEST_START_GRACE, recording, replaying, replaying_bounded, replaying_journalctl, sh_stand_in,
    spawning,
};

const STARTUP_RECORDING: &str = "leios-node-traces-startup.log";
const STARTUP_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces-startup.log");

/// A running node's stream rather than a starting one's, so every line in it
/// is a trace line.
const TRACE_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces.log");

#[test]
fn every_line_arrives_in_order_and_the_stream_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = replaying(replaying_journalctl(&dir, &recording(STARTUP_RECORDING)));

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, STARTUP_WINDOW.lines().collect::<Vec<_>>());
    // The end is an end, not an error: the caller's respawn decision hangs on
    // telling the two apart.
    assert_eq!(source.next_line().unwrap(), None);
}

// The status the child chose, not the signal this process would send it. 13 is
// arbitrary: what is asserted is that the number arrives, because the number is
// what tells a journalctl refused the journal from one that never resolved its
// unit. Which status a real journalctl picks is not recorded anywhere, so
// nothing here claims one.
#[test]
fn a_stream_that_ended_reports_the_status_its_journalctl_exited_with() {
    let dir = tempfile::tempdir().unwrap();
    let mut source = replaying(sh_stand_in(&dir, "exiting-journalctl", "exit 13"));
    while source.next_line().unwrap().is_some() {}

    let end = source.reap().to_string();

    assert!(
        end.contains("13"),
        "the end has to carry journalctl's own status, got: {end}"
    );
}

// The misconfigured-unit shape: journalctl waits and never exits, so this
// process ends it and what it reports is that kill.
#[test]
fn a_stream_still_running_is_ended_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    // A stand-in that cannot reach an exit of its own. One that writes a
    // recording would race instead: the whole of it fits in a pipe buffer, so it
    // can finish and exit 0 before this line runs.
    let source = replaying(sh_stand_in(&dir, "waiting-journalctl", "exec sleep 3600"));

    let end = source.stop().to_string();

    assert!(
        end.contains("signal"),
        "a journalctl this process ended has no status of its own, got: {end}"
    );
}

// The SupplementaryGroups case `Spawned::confirm_following` is for: 13 is the
// stand-in's status arriving through `StartError::NotFollowing`.
#[test]
fn a_journalctl_refused_the_journal_fails_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let spawned = spawning(sh_stand_in(&dir, "refused-journalctl", "exit 13"));

    let Err(error) = spawned.confirm_following() else {
        panic!("a journalctl that exited is not following the unit");
    };

    assert!(error.to_string().contains("13"), "{error}");
    assert!(error.to_string().contains("refused-journalctl"), "{error}");
}

// The healthy shape: journalctl follows and writes nothing until the node
// does, so starting must not wait for a line.
#[test]
fn a_journalctl_still_following_starts() {
    let dir = tempfile::tempdir().unwrap();
    let spawned = spawning(sh_stand_in(&dir, "silent-journalctl", "exec sleep 3600"));

    spawned
        .confirm_following()
        .expect("a journalctl that is still running is following");
}

// A journalctl that is not where the config says fails at startup rather than
// leaving a thread quietly reading nothing.
#[test]
fn a_journalctl_that_is_not_there_fails_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let spawned = JournalSource::spawn(&JournalConfig {
        journal_unit: "cardano-node".to_string(),
        journalctl_path: dir.path().join("no-such-journalctl"),
        start_grace: TEST_START_GRACE,
        max_line_bytes: support::TEST_MAX_LINE_BYTES,
    });
    let Err(error) = spawned else {
        panic!("spawning a journalctl that is not there has to fail");
    };
    assert!(
        matches!(error, StartError::Spawn { .. }),
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
        String::from_utf8(self.bytes()).unwrap()
    }

    /// For the one case where what the node wrote is not text: the tee's whole
    /// contract is that it writes those bytes through unchanged.
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
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
        max_line_bytes: support::TEST_MAX_LINE_BYTES,
    }
}

/// The same tee with a bound a test can reach, so what it asserts about an
/// oversized line does not depend on writing 64 KiB of one.
fn bounded_queue(capacity: usize, max_line_bytes: usize) -> PipeConfig {
    PipeConfig {
        max_line_bytes: NonZeroUsize::new(max_line_bytes).unwrap(),
        ..queue_of(capacity)
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

/// A stdin interrupted by a signal between two lines, which is what a process
/// handling one looks like to the read underneath it.
struct InterruptedBetweenLines(u8);

impl std::io::Read for InterruptedBetweenLines {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        unreachable!("the tee reads through fill_buf")
    }
}

impl std::io::BufRead for InterruptedBetweenLines {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        self.0 += 1;
        match self.0 {
            1 => Ok(b"first\n"),
            2 => Err(std::io::Error::from(std::io::ErrorKind::Interrupted)),
            3 => Ok(b"second\n"),
            _ => Ok(b""),
        }
    }

    fn consume(&mut self, _: usize) {}
}

/// An interruption is not the node closing its output, and the tee reads on
/// through it. What it costs to get this wrong is the pipe, not the journal:
/// the tee thread stops draining stdin, which blocks the node's writes, and
/// the drop-in's restart policy then takes the node down with it.
#[test]
fn a_read_a_signal_interrupted_is_not_the_node_exiting() {
    let mut source = PipeSource::tee(InterruptedBetweenLines(0), Vec::new(), &queue_of(64));

    assert_eq!(source.next_line().unwrap(), Some("first".to_string()));
    assert_eq!(source.next_line().unwrap(), Some("second".to_string()));
    assert_eq!(
        source.next_line().unwrap(),
        None,
        "the stream ended by reading zero bytes, which is the only end there is"
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
    let error = log_section("source = \"pipe\"\nstart_grace_secs = 2").unwrap_err();
    assert!(error.contains("start_grace_secs"), "{error}");
    let error = log_section(
        "source = \"journald\"\njournal_unit = \"cardano-node\"\n\
         journalctl_path = \"/usr/bin/journalctl\"\npipe_queue_capacity = 8",
    )
    .unwrap_err();
    assert!(error.contains("pipe_queue_capacity"), "{error}");
}

// A grace of nothing would confirm nothing: the child has not been scheduled
// yet, so it is still running whatever the journal did.
#[test]
fn a_start_grace_of_zero_fails_loudly() {
    let error = log_section(
        "source = \"journald\"\njournal_unit = \"cardano-node\"\n\
         journalctl_path = \"/usr/bin/journalctl\"\nstart_grace_secs = 0",
    )
    .unwrap_err();
    assert!(error.contains("start_grace_secs"), "{error}");
}

// A queue of nothing would drop every line, so the config refuses it rather
// than collecting nothing quietly.
#[test]
fn a_queue_capacity_of_zero_fails_loudly() {
    let error = log_section("source = \"pipe\"\npipe_queue_capacity = 0").unwrap_err();
    assert!(error.contains("pipe_queue_capacity"), "{error}");
}

/// The bound is on what this process keeps, never on what the node's reader
/// gets: a line too long to ship is still written through byte for byte, and
/// the lines around it are unaffected.
#[test]
fn an_oversized_line_is_written_through_and_not_offered() {
    let long = "x".repeat(64);
    let output = format!("short\n{long}\nafter\n");
    let downstream = Downstream::default();
    let mut source = PipeSource::tee(
        std::io::Cursor::new(output.clone()),
        downstream.clone(),
        &bounded_queue(64, 16),
    );

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    // The line after it is the point: reading the oversized line whole is what
    // keeps the stream in sync, so its tail is not read as the next line.
    assert_eq!(read, ["short", "after"]);
    assert_eq!(downstream.written(), output);
}

/// The bound is the line's own bytes, terminator excluded, so a line exactly
/// that long is inside it.
#[test]
fn a_line_the_length_of_the_bound_is_kept() {
    let exact = "x".repeat(16);
    let over = "x".repeat(17);
    let mut source = PipeSource::tee(
        std::io::Cursor::new(format!("{exact}\n{over}\n")),
        Downstream::default(),
        &bounded_queue(64, 16),
    );

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, [exact]);
}

/// A line ending `\r\n` is measured after the terminator comes off, not
/// refused for carrying one.
#[test]
fn a_carriage_return_is_not_counted_against_the_bound() {
    let exact = "x".repeat(16);
    let mut source = PipeSource::tee(
        std::io::Cursor::new(format!("{exact}\r\n")),
        Downstream::default(),
        &bounded_queue(64, 16),
    );

    assert_eq!(source.next_line().unwrap(), Some(exact));
}

/// One line the node did not write as UTF-8 costs that line and nothing else.
/// Read into a `String` it was a read failure, which stops the tee reading
/// stdin at all, and a tee that stops reading is what blocks the node.
#[test]
fn a_line_that_is_not_utf8_does_not_stop_the_tee() {
    let mut output = b"first\n".to_vec();
    output.extend_from_slice(&[0xff, 0xfe, b'\n']);
    output.extend_from_slice(b"third\n");
    let downstream = Downstream::default();
    let mut source = PipeSource::tee(
        std::io::Cursor::new(output.clone()),
        downstream.clone(),
        &queue_of(64),
    );

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    // Nothing is dropped and nothing is elided: the two bytes are each one
    // replacement character, and the lines either side are untouched. Such a
    // line is not valid JSON either way, so what it costs is the substitution
    // and not the line.
    assert_eq!(read, ["first", "\u{fffd}\u{fffd}", "third"]);
    // And the node's reader still gets the bytes it wrote, not the
    // replacement characters.
    assert_eq!(downstream.bytes(), output);
}

/// The journal source drops an oversized line the same way, and reading it
/// whole is what leaves the following line intact. A stand-in writes the three
/// lines, so what is exercised is the reading rather than the flags.
#[test]
fn the_journal_source_skips_an_oversized_line_and_reads_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let long = "x".repeat(64);
    let stand_in = sh_stand_in(
        &dir,
        "long-line-journalctl",
        &format!("printf 'short\\n{long}\\nafter\\n'"),
    );
    let mut source = replaying_bounded(stand_in, NonZeroUsize::new(16).unwrap());

    let mut read = Vec::new();
    while let Some(line) = source.next_line().unwrap() {
        read.push(line);
    }

    assert_eq!(read, ["short", "after"]);
}

/// What the report has to carry, because it is what the remedy needs: an
/// operator meeting this line either raises the bound or excludes the
/// namespace, and cannot do the second without its name.
///
/// Against the longest recorded line that is a trace envelope, rather than the
/// longest recorded line: that one is the node's plain-text configuration
/// dump, which has no namespace to report and is why the message promises a
/// head rather than a namespace.
#[test]
fn an_oversized_line_is_reported_by_the_namespace_it_came_from() {
    let longest = TRACE_WINDOW
        .lines()
        .filter(|line| metsuke_wire::envelope::TraceLine::parse(line).is_ok())
        .max_by_key(|line| line.len())
        .expect("the recording has trace lines");
    let namespace = metsuke_wire::envelope::TraceLine::parse(longest)
        .ok()
        .and_then(|line| {
            metsuke::logselect::Fields::of(&line)
                .namespace
                .map(str::to_string)
        })
        .expect("a recorded line carries a namespace");

    let head = metsuke::logsource::reported_head(longest);

    assert!(
        head.contains(&namespace),
        "the report names {namespace} nowhere: {head}"
    );
    // Bounded, so one line cannot become a screenful in the journal.
    assert!(head.len() < longest.len(), "{} bytes", head.len());
}
