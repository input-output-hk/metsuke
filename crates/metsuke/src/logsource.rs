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

use metsuke_wire::journal::{ERR, WARNING};

/// A continuous stream of lines, so a second transport is a second
/// implementation rather than a second loop.
pub trait LineSource {
    /// The next line without its terminator, or `None` when the stream ended.
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct JournalConfig {
    /// The systemd unit the node runs as.
    pub journal_unit: String,
    /// Which journalctl to run. The shipped unit names an absolute store
    /// path, because a hardened unit's PATH is not something to rely on.
    pub journalctl_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PipeConfig {
    /// Lines the tee may hold for the parse-and-spool worker. Full means the
    /// line is dropped, so this is how much of a stall in spooling the stream
    /// absorbs before collection loses lines.
    pub queue_capacity: NonZeroUsize,
}

#[derive(Debug, thiserror::Error)]
pub enum LineSourceError {
    #[error("cannot start {path}: {source}")]
    Spawn {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("reading from {path} failed: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

pub struct JournalSource {
    /// `None` once `reap` or `stop` has taken it, so `drop` does not wait on a
    /// child one of them already reaped.
    child: Option<Child>,
    lines: BufReader<ChildStdout>,
    path: String,
}

/// How the journalctl behind a stream stopped: the status it chose, or why this
/// process could not find out.
///
/// Kept because a journalctl refused the journal read exits on its own, while
/// one following a unit that does not resolve waits forever — two ends the
/// reading side cannot tell apart, with different remedies.
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
    pub fn spawn(config: &JournalConfig) -> Result<Self, LineSourceError> {
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
            .map_err(|source| LineSourceError::Spawn {
                path: path.clone(),
                source,
            })?;
        let stdout = child
            .stdout
            .take()
            .expect("stdout is piped, so the handle is there");
        Ok(JournalSource {
            child: Some(child),
            lines: BufReader::new(stdout),
            path,
        })
    }

    /// The status of a journalctl whose output has ended.
    ///
    /// Blocking, and no kill: stdout closes as the child exits, so `try_wait`
    /// can still answer `None` for the moment in between, and killing on that
    /// answer would report this process's signal in place of the status the
    /// child chose.
    pub fn reap(mut self) -> ChildEnd {
        waited(self.taken())
    }

    /// The same for a stream that stopped without ending, where the child may
    /// still be running and following the unit.
    pub fn stop(mut self) -> ChildEnd {
        let mut child = self.taken();
        match child.kill() {
            Ok(()) => waited(child),
            Err(error) => ChildEnd::Unavailable(error),
        }
    }

    /// Both callers consume the source, so the child is still here.
    fn taken(&mut self) -> Child {
        self.child
            .take()
            .expect("a source hands its child to reap or stop once")
    }
}

fn waited(mut child: Child) -> ChildEnd {
    match child.wait() {
        Ok(status) => ChildEnd::Status(status),
        Err(error) => ChildEnd::Unavailable(error),
    }
}

impl LineSource for JournalSource {
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError> {
        let mut line = String::new();
        let read = self
            .lines
            .read_line(&mut line)
            .map_err(|source| LineSourceError::Read {
                path: self.path.clone(),
                source,
            })?;
        if read == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\r', '\n']).to_string()))
    }
}

impl Drop for JournalSource {
    /// A source that is dropped because its stream failed leaves a journalctl
    /// behind otherwise, and the respawn would then have two following the
    /// same unit.
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
        std::thread::spawn(move || tee_through(input, output, sender, counted, reason));
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
            Some(source) => Err(LineSourceError::Read {
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
) {
    let mut writing = true;
    loop {
        let mut line = String::new();
        match input.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("{ERR}reading the node's output failed: {error}");
                *ended.lock().expect("no reader panics holding this") = Some(error);
                break;
            }
        }
        if writing
            && let Err(error) = output
                .write_all(line.as_bytes())
                .and_then(|()| output.flush())
        {
            eprintln!(
                "{ERR}writing the node's output through failed, \
                 whatever reads it downstream is no longer getting it: {error}"
            );
            writing = false;
        }
        let offered = line.trim_end_matches(['\r', '\n']).to_string();
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
    let dropped = dropped.load(Ordering::Relaxed);
    if dropped > 0 {
        eprintln!("{WARNING}the trace-line queue dropped {dropped} lines in all");
    }
}
