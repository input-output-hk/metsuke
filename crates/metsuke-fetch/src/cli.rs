//! CLI arguments, hand-parsed: a handful of flags do not earn a parser
//! dependency on a binary that holds the pull credential.
//!
//! No access flag has a default. The endpoint, the account and the deadline are
//! the deployment's, and a tool that guessed one would sync from somewhere the
//! operator did not name. A variable is not a guess: it is the operator naming
//! the same deployment once instead of per run.
//!
//! The environment is passed in rather than read here, so a run's answer never
//! depends on what the machine happens to export.

use std::num::NonZeroU64;
use std::path::PathBuf;

use metsuke_wire::envelope::{AgentId, PoolId};
use metsuke_wire::key::{KEY_PREFIX, Kind};

use crate::select::Selection;
use crate::sync::{Insist, Verification};

/// What one object may weigh before this run refuses to hold it to check it.
/// Sixteen times the shipped `max_body_bytes`, so the ceiling is the tool's
/// own safety net and never the thing an operator meets.
const DEFAULT_MAX_OBJECT_BYTES: NonZeroU64 = NonZeroU64::new(16 * 1024 * 1024).unwrap();

/// The build, which every run names and `--version` answers with. What it
/// promises across builds is in docs/releasing.md.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What stands in for each access flag. Named once, so `USAGE`, the fallback
/// and the refusal cannot name three different sets.
///
/// `ENV_PASSWORD_FILE` carries the path, as its flag does: a password in the
/// environment reaches every child and anything that may read /proc.
pub const ENV_SERVER: &str = "METSUKE_FETCH_SERVER";
pub const ENV_USER: &str = "METSUKE_FETCH_USER";
pub const ENV_PASSWORD_FILE: &str = "METSUKE_FETCH_PASSWORD_FILE";
pub const ENV_TIMEOUT_MS: &str = "METSUKE_FETCH_TIMEOUT_MS";

/// What `--help` answers. A refusal points here instead of repeating it, so
/// the error an operator has to read stays one line.
pub const USAGE: &str = "metsuke-fetch downloads the signed telemetry archive to local files.

usage:
  metsuke-fetch list <access> [filters]
  metsuke-fetch sync <access> [filters] --state <path> --into <dir>
  metsuke-fetch --help | --version

  list   print the keys the filters match, downloading nothing
  sync   download the ones the cursor has not seen, then advance the cursor

access, which every command needs:
  --server <url>          where the archive is
  --user <name>           the developer account to authenticate as
  --password-file <path>  a file holding that account's password, nothing else
  --timeout-ms <n>        how long one request may take, in milliseconds

sync:
  --state <path>          the cursor; a run resumes from it and advances it
  --into <dir>            where objects land, each under its own key
  --max-object-bytes <n>  the largest size each object can be;
                          16777216 by default
  --require-attested      write only cold-signed and Leios-signed objects
  --require-cold-signed   write only cold-signed objects

  One state file per set of filters. They may share one --into.

filters, which default to the whole archive:
  --prefix <key prefix>   only keys starting with this
  --pool <bech32>         only this pool, as the archive keys it
  --agent <id>            only this agent
  --kind metrics|logs     only this kind

No access flag has a default. Each also reads METSUKE_FETCH_<FLAG>, its own
name upper-cased with dashes as underscores, and the flag wins where both are
given. A variable set to nothing counts as unset, and the password variable
carries the path, never the password.

example:
  export METSUKE_FETCH_SERVER=https://archive.example
  export METSUKE_FETCH_USER=dev
  export METSUKE_FETCH_PASSWORD_FILE=~/.config/metsuke/password
  export METSUKE_FETCH_TIMEOUT_MS=30000
  metsuke-fetch sync --state ~/.local/state/metsuke-fetch/cursor.json \\
    --into ~/archive";

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
    /// What the run will hold to check an object, and whether it insists on
    /// checking one at all (`sync::Verification`).
    pub verification: Verification,
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
    /// Its own variant because an access flag has a second way to arrive, and
    /// naming only the flag sends an operator who set the variable looking in
    /// the wrong place.
    #[error("{command} needs {flag}, or {variable} in the environment\n{HELP_HINT}")]
    MissingAccess {
        command: &'static str,
        flag: &'static str,
        variable: &'static str,
    },
    #[error("{command} takes no {flag}\n{HELP_HINT}")]
    NotForCommand {
        command: &'static str,
        flag: &'static str,
    },
    #[error("{flag} takes a whole number of bytes above zero, not {value:?}")]
    NotAByteCount { flag: &'static str, value: String },
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
    /// Parse the arguments after the program name, with `env` answering for
    /// the access flags none of them carried.
    pub fn parse(
        args: impl Iterator<Item = String>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Invocation, ArgsError> {
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
            // Each raises the bar and neither contradicts the other, so both
            // given is the higher one rather than a refusal.
            if let Some(insist) = match argument.as_str() {
                "--require-attested" => Some(Insist::Attested),
                "--require-cold-signed" => Some(Insist::ColdSigned),
                _ => None,
            } {
                given.insist = given.insist.max(insist);
                continue;
            }
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
                "--max-object-bytes" => ("--max-object-bytes", &mut given.max_object_bytes),
                _ => return Err(ArgsError::Unknown { argument }),
            };
            *value = Some(args.next().ok_or(ArgsError::MissingValue { flag })?);
        }
        given.fill_from(env);
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
    max_object_bytes: Option<String>,
    insist: Insist,
}

impl Given {
    /// The access flags nobody gave, from the environment. An empty value is
    /// taken as unset: an exported-but-empty variable is not an endpoint.
    fn fill_from(&mut self, env: impl Fn(&str) -> Option<String>) {
        for (value, variable) in [
            (&mut self.server, ENV_SERVER),
            (&mut self.user, ENV_USER),
            (&mut self.password_file, ENV_PASSWORD_FILE),
            (&mut self.timeout_ms, ENV_TIMEOUT_MS),
        ] {
            if value.is_none() {
                *value = env(variable).filter(|found| !found.is_empty());
            }
        }
    }

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
            for (flag, value) in [
                ("--state", &self.state),
                ("--into", &self.into),
                ("--max-object-bytes", &self.max_object_bytes),
            ] {
                if value.is_some() {
                    return Err(ArgsError::NotForCommand {
                        command: command.name(),
                        flag,
                    });
                }
            }
            // The same refusal, for the flag that carries no value to test.
            if self.insist != Insist::Nothing {
                return Err(ArgsError::NotForCommand {
                    command: command.name(),
                    flag: "--require-attested",
                });
            }
        }
        let name = command.name();
        let timeout_ms = access(&self.timeout_ms, name, "--timeout-ms", ENV_TIMEOUT_MS)?;
        Ok(Args {
            access: Access {
                server: access(&self.server, name, "--server", ENV_SERVER)?.to_string(),
                user: access(&self.user, name, "--user", ENV_USER)?.to_string(),
                password_file: PathBuf::from(access(
                    &self.password_file,
                    name,
                    "--password-file",
                    ENV_PASSWORD_FILE,
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
            verification: Verification {
                max_object_bytes: match self.max_object_bytes.as_deref() {
                    None => DEFAULT_MAX_OBJECT_BYTES,
                    Some(given) => given.parse().map_err(|_| ArgsError::NotAByteCount {
                        flag: "--max-object-bytes",
                        value: given.to_string(),
                    })?,
                },
                insist: self.insist,
            },
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

/// `required`, for the flags a variable can also carry.
fn access<'a>(
    value: &'a Option<String>,
    command: &'static str,
    flag: &'static str,
    variable: &'static str,
) -> Result<&'a str, ArgsError> {
    value.as_deref().ok_or(ArgsError::MissingAccess {
        command,
        flag,
        variable,
    })
}
