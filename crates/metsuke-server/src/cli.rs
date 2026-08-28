//! CLI arguments, hand-parsed: one flag and one subcommand do not earn a
//! parser dependency.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke-server/config.toml";

pub const VERIFY_ARCHIVE: &str = "verify-archive";

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "metsuke-server accepts signed telemetry submissions and archives them.

usage:
  metsuke-server [--config <path>]
      serve: accept submissions, answer the developer routes, and render the
      operator's onboarding page at /
  metsuke-server verify-archive [--config <path>]
      re-verify every object already in the archive, report, and exit
  metsuke-server --help | --version

flags:
  --config <path>   the server's configuration, defaulting to
                    /etc/metsuke-server/config.toml

The listen address, the pool allowlist, the archive and the developer account
are all configuration. The S3 credentials are not: the server reads
AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY from its environment so the config
file stays readable in the open. docs/deploying.md is the procedure.";

/// Where a refusal sends the operator, rather than printing all of `USAGE` on
/// top of the error.
pub const HELP_HINT: &str = "try metsuke-server --help";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
    pub command: Command,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    VerifyArchive,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("unknown argument {argument:?}\n{HELP_HINT}")]
    Unknown { argument: String },
    #[error("--config needs a <path> value\n{HELP_HINT}")]
    MissingValue,
    #[error("{first} and {second} cannot both run: name one\n{HELP_HINT}")]
    TwoCommands { first: String, second: String },
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, ArgsError> {
        let mut config = PathBuf::from(DEFAULT_CONFIG_PATH);
        let mut args = args;
        // Which subcommand word was taken, so a second one is refused rather
        // than silently winning over the first.
        let mut named: Option<String> = None;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--config" => {
                    config = args
                        .next()
                        .map(PathBuf::from)
                        .ok_or(ArgsError::MissingValue)?;
                }
                VERIFY_ARCHIVE => match named {
                    Some(first) => {
                        return Err(ArgsError::TwoCommands {
                            first,
                            second: argument,
                        });
                    }
                    None => named = Some(argument),
                },
                _ => return Err(ArgsError::Unknown { argument }),
            }
        }
        let command = match named.as_deref() {
            Some(VERIFY_ARCHIVE) => Command::VerifyArchive,
            _ => Command::Serve,
        };
        Ok(Args { config, command })
    }
}
