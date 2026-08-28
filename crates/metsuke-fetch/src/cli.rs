//! CLI arguments, hand-parsed: a handful of flags do not earn a parser
//! dependency on a binary that holds the pull credential.
//!
//! No access flag has a default. The endpoint, the account and the deadline are
//! the deployment's, and a tool that guessed one would sync from somewhere the
//! operator did not name.

use std::num::NonZeroU64;
use std::path::PathBuf;

use metsuke_wire::envelope::{AgentId, PoolId};
use metsuke_wire::key::{KEY_PREFIX, Kind};

use crate::select::Selection;

/// The build, which every run names and `--version` answers with. What it
/// promises across builds is in docs/releasing.md.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What `--help` answers. A refusal points here instead of repeating it, so
/// the error an operator has to read stays one line.
pub const USAGE: &str = "metsuke-fetch downloads the signed telemetry archive to local files.

usage:
  metsuke-fetch list <access> [filters]
      print the keys the filters match, downloading nothing
  metsuke-fetch sync <access> [filters] --state <path> --into <dir>
      download the ones the cursor has not seen, then advance it
  metsuke-fetch --help | --version

access, which every command needs and none of which has a default:
  --server <url>           where the archive is, e.g. https://archive.example
  --user <name>            the developer account the server was configured with
  --password-file <path>   a file holding that account's password and nothing else
  --timeout-ms <n>         how long one request may take, in milliseconds

sync:
  --state <path>           the cursor file; a run resumes from it and advances it
  --into <dir>             where objects land, each under its own key

filters, which default to the whole archive:
  --prefix <key prefix>    only keys starting with this
  --pool <pool1...>        only this pool, bech32 as the archive keys it
  --agent <id>             only this agent
  --kind metrics|logs      only this kind

example:
  metsuke-fetch sync --server https://archive.example --user dev \\
    --password-file ~/.config/metsuke/password --timeout-ms 30000 \\
    --state ~/.local/state/metsuke-fetch/cursor.json --into ~/archive";

/// Where a refusal sends the operator, rather than printing all of `USAGE` on
/// top of the error.
pub const HELP_HINT: &str = "try metsuke-fetch --help";

/// What the arguments asked for. `--help` and `--version` are their own
/// outcomes rather than commands, because they answer without an endpoint, an
/// account or a credential, and every command needs all three.
#[derive(Debug, PartialEq)]
pub enum Invocation {
    Help,
    Version,
    /// Boxed because `Args` dwarfs the other variant and this is parsed once
    /// per process.
    Run(Box<Args>),
}

#[derive(Debug, PartialEq)]
pub struct Args {
    pub command: Command,
    pub access: Access,
    /// The literal head of the object keys this run is about, which the server
    /// filters the listing on. An absent one is the archive's own prefix, so
    /// the two spellings of "everything" are one cursor.
    pub prefix: String,
    /// What the run keeps of the keys the prefix listed.
    pub selection: Selection,
}

/// What the run does with the keys it lists.
#[derive(Debug, PartialEq)]
pub enum Command {
    /// Print them and download nothing.
    List,
    /// Download the ones after the cursor, advancing it in `state`.
    Sync { state: PathBuf, into: PathBuf },
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::List => "list",
            Command::Sync { .. } => "sync",
        }
    }
}

/// How to reach the archive: where it is, who this is, and how long one
/// request may take.
#[derive(Debug, PartialEq)]
pub struct Access {
    pub server: String,
    pub user: String,
    /// A file holding the account's password and nothing else, as the server
    /// states it (`metsuke_server::config::DeveloperConfig`). A path, so no
    /// secret reaches the process table.
    pub password_file: PathBuf,
    pub timeout_ms: NonZeroU64,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("name a command, list or sync\n{HELP_HINT}")]
    NoCommand,
    #[error("unknown command {command:?}\n{HELP_HINT}")]
    UnknownCommand { command: String },
    #[error("unknown argument {argument:?}\n{HELP_HINT}")]
    Unknown { argument: String },
    #[error("{flag} needs a value\n{HELP_HINT}")]
    MissingValue { flag: &'static str },
    #[error("{command} needs {flag}\n{HELP_HINT}")]
    Missing {
        command: &'static str,
        flag: &'static str,
    },
    #[error("{command} takes no {flag}\n{HELP_HINT}")]
    NotForCommand {
        command: &'static str,
        flag: &'static str,
    },
    #[error("{flag} takes a whole number of milliseconds above zero, not {value:?}")]
    NotADuration { flag: &'static str, value: String },
    #[error("{flag} {value:?} is not one: {reason}")]
    NotAFilter {
        flag: &'static str,
        value: String,
        reason: String,
    },
}

impl Invocation {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Invocation, ArgsError> {
        // Asked for anywhere rather than in the command's place, because that
        // is where an operator writes them and a refusal there teaches nothing
        // the answer would not have.
        let args: Vec<String> = args.collect();
        if let Some(asked) = args.iter().find_map(|argument| match argument.as_str() {
            "--help" | "-h" => Some(Invocation::Help),
            "--version" => Some(Invocation::Version),
            _ => None,
        }) {
            return Ok(asked);
        }
        let mut args = args.into_iter();
        let command = args.next().ok_or(ArgsError::NoCommand)?;
        let mut given = Given::default();
        while let Some(argument) = args.next() {
            let (flag, value) = match argument.as_str() {
                "--server" => ("--server", &mut given.server),
                "--user" => ("--user", &mut given.user),
                "--password-file" => ("--password-file", &mut given.password_file),
                "--timeout-ms" => ("--timeout-ms", &mut given.timeout_ms),
                "--state" => ("--state", &mut given.state),
                "--into" => ("--into", &mut given.into),
                "--prefix" => ("--prefix", &mut given.prefix),
                "--pool" => ("--pool", &mut given.pool),
                "--agent" => ("--agent", &mut given.agent),
                "--kind" => ("--kind", &mut given.kind),
                _ => return Err(ArgsError::Unknown { argument }),
            };
            *value = Some(args.next().ok_or(ArgsError::MissingValue { flag })?);
        }
        given
            .into_args(&command)
            .map(|args| Invocation::Run(Box::new(args)))
    }
}

/// Every flag as it was given, before which command needs which is applied.
/// One field per flag so the value a repeated flag ends up with is the last
/// one, as every other tool reads a repeat.
#[derive(Default)]
struct Given {
    server: Option<String>,
    user: Option<String>,
    password_file: Option<String>,
    timeout_ms: Option<String>,
    state: Option<String>,
    into: Option<String>,
    prefix: Option<String>,
    pool: Option<String>,
    agent: Option<String>,
    kind: Option<String>,
}

impl Given {
    fn into_args(self, command: &str) -> Result<Args, ArgsError> {
        let command = match command {
            "list" => Command::List,
            "sync" => Command::Sync {
                state: PathBuf::from(required(&self.state, "sync", "--state")?),
                into: PathBuf::from(required(&self.into, "sync", "--into")?),
            },
            _ => {
                return Err(ArgsError::UnknownCommand {
                    command: command.to_string(),
                });
            }
        };
        // Refused rather than ignored: a `list --into` reads as a download
        // that would then write nothing.
        if let Command::List = command {
            for (flag, value) in [("--state", &self.state), ("--into", &self.into)] {
                if value.is_some() {
                    return Err(ArgsError::NotForCommand {
                        command: command.name(),
                        flag,
                    });
                }
            }
        }
        let name = command.name();
        let timeout_ms = required(&self.timeout_ms, name, "--timeout-ms")?;
        Ok(Args {
            access: Access {
                server: required(&self.server, name, "--server")?.to_string(),
                user: required(&self.user, name, "--user")?.to_string(),
                password_file: PathBuf::from(required(
                    &self.password_file,
                    name,
                    "--password-file",
                )?),
                timeout_ms: timeout_ms.parse().map_err(|_| ArgsError::NotADuration {
                    flag: "--timeout-ms",
                    value: timeout_ms.to_string(),
                })?,
            },
            prefix: match self.prefix.as_deref().unwrap_or_default() {
                "" => KEY_PREFIX.to_string(),
                asked => asked.to_string(),
            },
            selection: self.selection()?,
            command,
        })
    }

    /// The three key-segment filters, each parsed by whatever owns its form, so
    /// a typo is refused here rather than selecting nothing at the archive.
    fn selection(&self) -> Result<Selection, ArgsError> {
        let refuse = |flag: &'static str, value: &str, reason: String| ArgsError::NotAFilter {
            flag,
            value: value.to_string(),
            reason,
        };
        Ok(Selection {
            pool: self
                .pool
                .as_deref()
                .map(|pool| {
                    PoolId::from_bech32(pool)
                        .map_err(|error| refuse("--pool", pool, error.to_string()))
                })
                .transpose()?,
            agent: self
                .agent
                .as_deref()
                .map(|agent| {
                    AgentId::parse(agent)
                        .map_err(|error| refuse("--agent", agent, error.to_string()))
                })
                .transpose()?,
            kind: self
                .kind
                .as_deref()
                .map(|kind| {
                    Kind::parse(kind).ok_or_else(|| {
                        refuse(
                            "--kind",
                            kind,
                            format!(
                                "the kinds a key names are {} and {}",
                                Kind::Metrics,
                                Kind::Logs
                            ),
                        )
                    })
                })
                .transpose()?,
        })
    }
}

fn required<'a>(
    value: &'a Option<String>,
    command: &'static str,
    flag: &'static str,
) -> Result<&'a str, ArgsError> {
    value.as_deref().ok_or(ArgsError::Missing { command, flag })
}
