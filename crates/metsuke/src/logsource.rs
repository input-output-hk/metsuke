//! Where trace lines come from. `journalctl --follow` on the node's unit is the
//! one implementation; ADR 0010 weighs it against the pipe and prices the
//! `systemd-journal` grant it costs.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

/// A continuous stream of lines, so a second transport is a second
/// implementation rather than a second loop.
pub trait LineSource {
    /// The next line without its terminator, or `None` when the stream ended.
    fn next_line(&mut self) -> Result<Option<String>, LineSourceError>;
}

pub struct JournalConfig {
    /// The systemd unit the node runs as.
    pub journal_unit: String,
    /// Which journalctl to run. The shipped unit names an absolute store
    /// path, because a hardened unit's PATH is not something to rely on.
    pub journalctl_path: PathBuf,
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
    child: Child,
    lines: BufReader<ChildStdout>,
    path: String,
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
            child,
            lines: BufReader::new(stdout),
            path,
        })
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
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
