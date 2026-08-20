//! CLI arguments, hand-parsed: one flag does not earn a parser dependency.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke-server/config.toml";

pub const REBUILD_INDEX: &str = "rebuild-index";
pub const VERIFY_ARCHIVE: &str = "verify-archive";

/// Says the operator meant an archive with nothing in it, so `rebuild-index`
/// may seed nothing rather than refuse (`rebuild::EmptyArchive`).
pub const ALLOW_EMPTY: &str = "--allow-empty";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
    pub command: Command,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    /// `allow_empty` rides the variant it applies to: on `serve` or
    /// `verify-archive` the flag would have nothing to mean, and accepting it
    /// there would read as though it did something.
    RebuildIndex {
        allow_empty: bool,
    },
    VerifyArchive,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error(
        "unknown argument {argument:?} (usage: metsuke-server [--config <path>] \
         [{REBUILD_INDEX} [{ALLOW_EMPTY}]|{VERIFY_ARCHIVE}])"
    )]
    Unknown { argument: String },
    #[error("--config needs a <path> value")]
    MissingValue,
    #[error("{first} and {second} cannot both run: name one")]
    TwoCommands { first: String, second: String },
    #[error("{ALLOW_EMPTY} only means anything to {REBUILD_INDEX}")]
    AllowEmptyWithoutRebuild,
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, ArgsError> {
        let mut config = PathBuf::from(DEFAULT_CONFIG_PATH);
        // The flag may be written either side of the subcommand word, so both
        // are collected first and the command is built once from the pair.
        let mut allow_empty = false;
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
                ALLOW_EMPTY => allow_empty = true,
                REBUILD_INDEX | VERIFY_ARCHIVE => match named {
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
        let command = match (named.as_deref(), allow_empty) {
            (Some(REBUILD_INDEX), allow_empty) => Command::RebuildIndex { allow_empty },
            (_, true) => return Err(ArgsError::AllowEmptyWithoutRebuild),
            (Some(VERIFY_ARCHIVE), false) => Command::VerifyArchive,
            (_, false) => Command::Serve,
        };
        Ok(Args { config, command })
    }
}
