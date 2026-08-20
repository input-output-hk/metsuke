//! CLI arguments, hand-parsed: one flag does not earn a parser dependency.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke-server/config.toml";

pub const REBUILD_INDEX: &str = "rebuild-index";
pub const VERIFY_ARCHIVE: &str = "verify-archive";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
    pub command: Command,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    RebuildIndex,
    VerifyArchive,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error(
        "unknown argument {argument:?} (usage: metsuke-server [--config <path>] \
         [{REBUILD_INDEX}|{VERIFY_ARCHIVE}])"
    )]
    Unknown { argument: String },
    #[error("--config needs a <path> value")]
    MissingValue,
    #[error("{first} and {second} cannot both run: name one")]
    TwoCommands { first: String, second: String },
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, ArgsError> {
        let mut parsed = Args {
            config: PathBuf::from(DEFAULT_CONFIG_PATH),
            command: Command::Serve,
        };
        let mut args = args;
        // Which subcommand word was taken, so a second one is refused rather
        // than silently winning over the first.
        let mut named: Option<String> = None;
        while let Some(argument) = args.next() {
            let command = match argument.as_str() {
                "--config" => {
                    parsed.config = args
                        .next()
                        .map(PathBuf::from)
                        .ok_or(ArgsError::MissingValue)?;
                    continue;
                }
                REBUILD_INDEX => Command::RebuildIndex,
                VERIFY_ARCHIVE => Command::VerifyArchive,
                _ => return Err(ArgsError::Unknown { argument }),
            };
            if let Some(first) = named {
                return Err(ArgsError::TwoCommands {
                    first,
                    second: argument,
                });
            }
            named = Some(argument);
            parsed.command = command;
        }
        Ok(parsed)
    }
}
