//! Where trace lines come from: `journalctl --follow` on the node's unit, or
//! the node's own stdout piped into this process. ADR 0010 weighs the two and
//! prices the `systemd-journal` grant the journal costs. Which one runs is
//! `[log].source`, never inferred from the shape of stdin.

use std::io::{BufRead, BufReader, Write};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metsuke_wire::journal::{ERR, WARNING};

/// A continuous stream of lines, so a second transport is a second
/// implementation rather than a second loop.
pub trait LineSource {
    /// The next line without its terminator, or `None` when the stream ended.
    ///
    /// A line past `max_line_bytes` is never handed back: what the node writes
    /// decides how much of it this process holds, so the bound belongs where
    /// the bytes arrive rather than at the row built from them. Each source
    /// counts and reports its own.
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError>;
}

/// One line, read whole and kept in part.
///
/// Reading it whole is what keeps the stream in sync: the bytes past the bound
/// are on their way either way, and stopping short of the terminator would
/// make the rest of one line into the start of the next.
enum Line {
    /// A line inside the bound, its terminator already off.
    Kept(String),
    /// Past the bound, with what it measured and enough of its head to say
    /// where it came from.
    ///
    /// Nothing of it is shipped. Truncating it to the bound instead would
    /// change nothing about that: a prefix of a trace line is not valid JSON,
    /// so `logselect::select` refuses it as `NotAnObject` and skips it
    /// silently, which is the same loss with no report and one more copy of
    /// the line to make it. What the head is for is the operator, whose
    /// remedies are raising the bound or excluding the namespace, and both
    /// need to know which namespace this was.
    TooLong { bytes: u64, head: String },
    /// The stream ended.
    Ended,
}

/// Read one line, hand every byte of it to `through`, and keep at most `max`
/// of them.
///
/// `max` is the line's own bytes, terminator excluded, which is what a source
/// offers and what the spool stores. Two bytes over that are kept so a line
/// ending `\r\n` is measured after the terminator comes off rather than
/// refused for carrying one.
fn read_bounded(
    input: &mut impl BufRead,
    max: usize,
    mut through: impl FnMut(&[u8]),
) -> std::io::Result<Line> {
    let mut kept: Vec<u8> = Vec::new();
    let room = max.saturating_add(2);
    let mut bytes = 0u64;
    loop {
        let available = match input.fill_buf() {
            Ok(available) => available,
            // A signal arriving mid-read is not the stream ending, and this
            // is the one caller that cannot treat it as one: on the pipe the
            // read failing stops the tee, and a process that stops reading a
            // pipe is what fills the node's write buffer and blocks it.
            // `BufRead::read_line` retries for the same reason, and reading
            // by hand is what gave that up.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            break;
        }
        // Up to and including the terminator, so `consume` never leaves part
        // of a line behind and never takes the start of the next one.
        let (chunk, ended) = match available.iter().position(|byte| *byte == b'\n') {
            Some(at) => (&available[..=at], true),
            None => (available, false),
        };
        through(chunk);
        bytes += chunk.len() as u64;
        if kept.len() < room {
            let take = (room - kept.len()).min(chunk.len());
            kept.extend_from_slice(&chunk[..take]);
        }
        let consumed = chunk.len();
        input.consume(consumed);
        if ended {
            return Ok(bounded(kept, max, bytes));
        }
    }
    match bytes {
        // A last line the stream ended without terminating.
        0 => Ok(Line::Ended),
        _ => Ok(bounded(kept, max, bytes)),
    }
}

/// How much of an oversized line's head is copied out to name it in the
/// journal. Shapes how a loss is reported and nothing else, so it is not
/// configuration (CLAUDE.md `## Conventions`): what is dropped is the same
/// line whatever this is.
///
/// Wide enough for a trace envelope's `ns`, which every recorded line carries
/// inside its first 40 bytes, so an operator can decide whether to raise the
/// bound or exclude the namespace. Not every long line has one to find: the
/// longest line any recording holds is the node's plain-text configuration
/// dump at startup, which is not an envelope at all, and a head is the only
/// thing that could tell an operator that. `one_line` bounds what goes out.
const HEAD_BYTES: usize = 400;

/// An oversized line's head, as a journal line: bounded, on a character
/// boundary, and with the node's control characters mapped to spaces, because
/// this reaches a terminal running `journalctl` and until here the bytes were
/// the node's to choose.
fn head_of(line: &[u8]) -> String {
    let head = &line[..line.len().min(HEAD_BYTES)];
    metsuke_wire::http::one_line(String::from_utf8_lossy(head).into_owned())
}

/// `head_of` against a real line, because what it has to carry is the
/// envelope's `ns` and nothing in the type says so.
#[cfg(feature = "test-support")]
pub fn reported_head(line: &str) -> String {
    head_of(line.as_bytes())
}

/// What was kept, once the terminator is off and the bound is applied to what
/// is left.
///
/// `kept` holds the whole line whenever the line is inside the bound, so the
/// length compared here is the line's own and not the buffer's ceiling.
fn bounded(kept: Vec<u8>, max: usize, bytes: u64) -> Line {
    let trimmed = match kept
        .iter()
        .rposition(|byte| *byte != b'\n' && *byte != b'\r')
    {
        Some(last) => &kept[..=last],
        None => &[][..],
    };
    if trimmed.len() > max {
        return Line::TooLong {
            bytes,
            head: head_of(trimmed),
        };
    }
    // Lossy, so a line the node did not write as UTF-8 costs that line and
    // nothing else. Read into a `String` it was `InvalidData`, which is a read
    // failure to both callers: the journal source loses the stream until the
    // next respawn, and the tee stops reading stdin at all, which is what
    // fills the node's write buffer and blocks it.
    Line::Kept(String::from_utf8_lossy(trimmed).into_owned())
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalConfig {
    /// The systemd unit the node runs as.
    pub journal_unit: String,
    /// Which journalctl to run. The shipped unit names an absolute store
    /// path, because a hardened unit's PATH is not something to rely on.
    pub journalctl_path: PathBuf,
    /// How long a spawned journalctl has to still be running before it counts
    /// as following (semantics: `Spawned::confirm_following`).
    pub start_grace: Duration,
    /// The most of one line this process holds (semantics: `read_bounded`).
    pub max_line_bytes: NonZeroUsize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipeConfig {
    /// Lines the tee may hold for the parse-and-spool worker. Full means the
    /// line is dropped, so this is how much of a stall in spooling the stream
    /// absorbs before collection loses lines.
    pub queue_capacity: NonZeroUsize,
    /// The most of one line this process holds (semantics: `read_bounded`).
    /// With `queue_capacity`, the product is what a queue full of lines at the
    /// bound costs.
    pub max_line_bytes: NonZeroUsize,
}

/// Why a journal source never started. Separate from `LineSourceError`, which
/// only a source that did start can hand back.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("cannot start {path}: {source}")]
    Spawn {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} stopped instead of following the journal ({end})")]
    NotFollowing { path: String, end: ChildEnd },
}

#[derive(Debug, thiserror::Error)]
#[error("reading from {path} failed: {source}")]
pub struct LineSourceError {
    pub path: String,
    #[source]
    pub source: std::io::Error,
}

pub struct JournalSource {
    child: ChildGuard,
    lines: BufReader<ChildStdout>,
    path: String,
    max_line_bytes: NonZeroUsize,
    oversized: u64,
}

/// A journalctl that is killed if it is dropped. Nothing else ends a
/// `--follow`, and one left behind means the respawn has two of them following
/// the same unit.
struct ChildGuard(Option<Child>);

/// `None` only after `reap` or `stop` took the child, and both consume the
/// source holding the guard.
const HELD: &str = "a guard holds its child until reap or stop takes it";

impl ChildGuard {
    fn held(&mut self) -> &mut Child {
        self.0.as_mut().expect(HELD)
    }

    fn taken(&mut self) -> Child {
        self.0.take().expect(HELD)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0
            && child.kill().is_ok()
        {
            // Only after a kill that landed: waiting on one still running is
            // this process stopping until journalctl does.
            let _ = child.wait();
        }
    }
}

/// How the journalctl behind a stream stopped: the status it chose, or why this
/// process could not find out.
///
/// Kept because a journalctl refused the journal read exits on its own, while
/// one following a unit that does not resolve waits forever. The reading side
/// cannot tell these two ends apart, and they call for different remedies.
#[derive(Debug)]
pub enum ChildEnd {
    Status(std::process::ExitStatus),
    Unavailable(std::io::Error),
}

impl std::fmt::Display for ChildEnd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChildEnd::Status(status) => write!(f, "journalctl {status}"),
            ChildEnd::Unavailable(error) => {
                write!(f, "journalctl's own exit status is unavailable: {error}")
            }
        }
    }
}

impl JournalSource {
    /// Follow the unit from the journal's current end. Nothing already in the
    /// journal is read: there is no resume mark yet, so an agent that restarts
    /// picks up from now rather than re-shipping whatever the journal still
    /// holds.
    pub fn spawn(config: &JournalConfig) -> Result<Spawned, StartError> {
        let path = config.journalctl_path.display().to_string();
        let mut child = Command::new(&config.journalctl_path)
            .args([
                "--follow",
                "--lines=0",
                "--no-pager",
                // MESSAGE alone: the node's own line, byte for byte, with no
                // journal framing around it.
                "--output=cat",
                "--unit",
            ])
            .arg(&config.journal_unit)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|source| StartError::Spawn {
                path: path.clone(),
                source,
            })?;
        let stdout = child
            .stdout
            .take()
            .expect("stdout is piped, so the handle is there");
        Ok(Spawned {
            child: ChildGuard(Some(child)),
            lines: BufReader::new(stdout),
            path,
            grace: config.start_grace,
            max_line_bytes: config.max_line_bytes,
        })
    }

    /// The status of a journalctl whose output has ended.
    ///
    /// Blocking, and no kill: stdout closes as the child exits, so `try_wait`
    /// can still answer `None` for the moment in between, and killing on that
    /// answer would report this process's signal in place of the status the
    /// child chose.
    pub fn reap(mut self) -> ChildEnd {
        self.report_oversized();
        waited(self.child.taken())
    }

    /// What the bound cost this stream, once it has ended. Printed beside the
    /// line naming why the stream stopped, so the two are read together.
    fn report_oversized(&self) {
        if self.oversized > 0 {
            eprintln!(
                "{WARNING}{} trace lines were past max_line_bytes and dropped",
                self.oversized
            );
        }
    }

    /// The same for a stream that stopped without ending, where the child may
    /// still be running and following the unit.
    pub fn stop(mut self) -> ChildEnd {
        self.report_oversized();
        let mut child = self.child.taken();
        match child.kill() {
            Ok(()) => waited(child),
            Err(error) => ChildEnd::Unavailable(error),
        }
    }
}

/// A journalctl that has execed, before anything says it is following the
/// unit. Reaching the stream without `confirm_following`'s wait takes a second
/// call, `unconfirmed`, which is not compiled into a shipped binary; dropping
/// this instead kills the child.
pub struct Spawned {
    child: ChildGuard,
    lines: BufReader<ChildStdout>,
    path: String,
    grace: Duration,
    max_line_bytes: NonZeroUsize,
}

impl Spawned {
    /// The stream, once the child is still there after the configured grace.
    ///
    /// journalctl execs before it opens the journal, so one the journal refuses
    /// exits after a spawn that already succeeded: the missing
    /// `SupplementaryGroups=systemd-journal` of ADR 0010, which an operator can
    /// fix. Nothing is read to find out: `--follow --lines=0` writes nothing
    /// until the node does, so the child's own exit is the only answer there is
    /// now. The grace is therefore a ceiling on what this catches: a refusal
    /// that takes longer than the grace starts the agent anyway, and reaches
    /// the operator as the stream ending once per respawn.
    pub fn confirm_following(mut self) -> Result<JournalSource, StartError> {
        std::thread::sleep(self.grace);
        match self.child.held().try_wait() {
            Ok(None) => Ok(self.source()),
            Ok(Some(status)) => Err(StartError::NotFollowing {
                path: self.path.clone(),
                end: ChildEnd::Status(status),
            }),
            Err(error) => Err(StartError::NotFollowing {
                path: self.path.clone(),
                end: ChildEnd::Unavailable(error),
            }),
        }
    }

    /// The stream without the start check (`confirm_following`).
    #[cfg(feature = "test-support")]
    pub fn unconfirmed(self) -> JournalSource {
        self.source()
    }

    fn source(self) -> JournalSource {
        JournalSource {
            child: self.child,
            lines: self.lines,
            path: self.path,
            max_line_bytes: self.max_line_bytes,
            oversized: 0,
        }
    }
}

fn waited(mut child: Child) -> ChildEnd {
    match child.wait() {
        Ok(status) => ChildEnd::Status(status),
        Err(error) => ChildEnd::Unavailable(error),
    }
}

impl LineSource for JournalSource {
    /// Loops past a line over the bound rather than answering for it: an
    /// oversized line is this one line's loss, and reporting it as either the
    /// stream ending or a read failing would cost the stream.
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError> {
        loop {
            let read = read_bounded(&mut self.lines, self.max_line_bytes.get(), |_| {}).map_err(
                |source| LineSourceError {
                    path: self.path.clone(),
                    source,
                },
            )?;
            match read {
                Line::Ended => return Ok(None),
                Line::Kept(line) => return Ok(Some(line)),
                Line::TooLong { bytes, head } => {
                    if self.oversized == 0 {
                        oversized_line(bytes, self.max_line_bytes, &head);
                    }
                    self.oversized += 1;
                }
            }
        }
    }
}

/// Said once per stream, because a node emitting one of these emits them at
/// whatever rate it emits that namespace, and the remedy is the same every
/// time: either the setting is too low for this node or the namespace it comes
/// from is one to exclude.
fn oversized_line(bytes: u64, max: NonZeroUsize, head: &str) {
    eprintln!(
        "{WARNING}a line of {bytes} bytes is past max_line_bytes ({}) and is dropped; \
         its head is below, and further ones are counted and reported when the stream \
         ends: {head}",
        max.get()
    );
}

/// `cardano-node run | metsuke`: the node's stdout, teed through to this
/// process's stdout and handed to the caller as lines.
///
/// The node's write path is the tee thread and nothing else. It writes each
/// line through before offering it here, and offers it without waiting, so
/// neither this process's reader nor the spool behind it can ever stall the
/// node. A line the queue has no room for is dropped and counted.
pub struct PipeSource {
    lines: Receiver<String>,
    dropped: Arc<AtomicU64>,
    ended: Arc<Mutex<Option<std::io::Error>>>,
}

impl PipeSource {
    /// Tee this process's stdin to its stdout. Nothing else in the agent
    /// writes to stdout, so what a downstream consumer reads is the node's
    /// bytes.
    pub fn spawn(config: &PipeConfig) -> PipeSource {
        PipeSource::tee(BufReader::new(std::io::stdin()), std::io::stdout(), config)
    }

    pub fn tee(
        input: impl BufRead + Send + 'static,
        output: impl Write + Send + 'static,
        config: &PipeConfig,
    ) -> PipeSource {
        let (sender, lines) = sync_channel(config.queue_capacity.get());
        let dropped = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&dropped);
        let ended = Arc::new(Mutex::new(None));
        let reason = Arc::clone(&ended);
        let max_line_bytes = config.max_line_bytes;
        std::thread::spawn(move || {
            tee_through(input, output, sender, counted, reason, max_line_bytes)
        });
        PipeSource {
            lines,
            dropped,
            ended,
        }
    }

    /// Lines the queue had no room for since the tee started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl LineSource for PipeSource {
    /// `None` once the tee has ended and the queue is empty. Only a read that
    /// returned zero bytes is EOF: anything else ended the stream without the
    /// node having exited, and answering `None` for it would report a
    /// collection failure to the caller as the node stopping.
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError> {
        if let Ok(line) = self.lines.recv() {
            return Ok(Some(line));
        }
        match self
            .ended
            .lock()
            .expect("the tee thread never panics")
            .take()
        {
            None => Ok(None),
            Some(source) => Err(LineSourceError {
                path: STDIN.to_string(),
                source,
            }),
        }
    }
}

/// What a pipe read failure names, since there is no path to name.
const STDIN: &str = "the node's output on stdin";

/// Read, write through, offer. A write failure is reported once and then the
/// tee reads on without writing: stdin still has to be drained, because a
/// process that stops reading a pipe is what fills the node's write buffer and
/// blocks it. std ignores SIGPIPE for the same reason, so a downstream that
/// closed arrives here as an error rather than as a signal.
fn tee_through(
    mut input: impl BufRead,
    mut output: impl Write,
    lines: SyncSender<String>,
    dropped: Arc<AtomicU64>,
    ended: Arc<Mutex<Option<std::io::Error>>>,
    max_line_bytes: NonZeroUsize,
) {
    // Set once and never cleared, and only what is written downstream: a
    // failure there is reported once and the tee reads on.
    let mut failed_write: Option<std::io::Error> = None;
    let mut writing = true;
    let mut oversized = 0u64;
    loop {
        // Every byte through as it arrives, whatever the bound says about
        // keeping it: the write-through is the node's own output, and a line
        // this process will not ship is still a line its reader is owed.
        let read = read_bounded(&mut input, max_line_bytes.get(), |chunk| {
            if writing && let Err(error) = output.write_all(chunk) {
                failed_write = Some(error);
                writing = false;
            }
        });
        if let Some(error) = failed_write.take() {
            eprintln!(
                "{ERR}writing the node's output through failed, \
                 whatever reads it downstream is no longer getting it: {error}"
            );
        }
        if writing && let Err(error) = output.flush() {
            eprintln!(
                "{ERR}flushing the node's output through failed, \
                 whatever reads it downstream is no longer getting it: {error}"
            );
            writing = false;
        }
        let offered = match read {
            Ok(Line::Ended) => break,
            Ok(Line::Kept(line)) => line,
            Ok(Line::TooLong { bytes, head }) => {
                if oversized == 0 {
                    oversized_line(bytes, max_line_bytes, &head);
                }
                oversized += 1;
                continue;
            }
            Err(error) => {
                eprintln!("{ERR}reading the node's output failed: {error}");
                *ended.lock().expect("no reader panics holding this") = Some(error);
                break;
            }
        };
        if let Err(TrySendError::Full(_)) = lines.try_send(offered) {
            let before = dropped.fetch_add(1, Ordering::Relaxed);
            if before == 0 {
                eprintln!(
                    "{WARNING}the trace-line queue is full and lines are being dropped; \
                     the node is never waited on"
                );
            }
        }
    }
    if oversized > 0 {
        eprintln!("{WARNING}{oversized} trace lines were past max_line_bytes and dropped");
    }
    let dropped = dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        eprintln!("{WARNING}the trace-line queue dropped {dropped} lines in all");
    }
}
