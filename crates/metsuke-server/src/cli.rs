//! CLI arguments, hand-parsed: one flag does not earn a parser dependency.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke-server/config.toml";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("unknown argument {argument:?} (usage: metsuke-server [--config <path>])")]
    Unknown { argument: String },
    #[error("--config needs a <path> value")]
    MissingValue,
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, ArgsError> {
        let mut parsed = Args {
            config: PathBuf::from(DEFAULT_CONFIG_PATH),
        };
        let mut args = args;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--config" => {
                    parsed.config = args
                        .next()
                        .map(PathBuf::from)
                        .ok_or(ArgsError::MissingValue)?;
                }
                _ => return Err(ArgsError::Unknown { argument }),
            }
        }
        Ok(parsed)
    }
}
