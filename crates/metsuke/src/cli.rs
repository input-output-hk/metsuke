//! CLI arguments, hand-parsed: two flags don't earn a parser dependency on
//! a binary whose attack surface is the product.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke/config.toml";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
    /// Overrides the config's `signing_key` (the LoadCredential interplay
    /// is documented in contrib/config.example.toml).
    pub signing_key: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error(
        "unknown argument {argument:?} (usage: metsuke [--config <path>] [--signing-key <path>])"
    )]
    Unknown { argument: String },
    #[error("{flag} needs a <path> value")]
    MissingValue { flag: &'static str },
}

impl Args {
    /// Parse the arguments after the program name.
    pub fn parse(args: impl Iterator<Item = String>) -> Result<Args, ArgsError> {
        let mut parsed = Args {
            config: PathBuf::from(DEFAULT_CONFIG_PATH),
            signing_key: None,
        };
        let mut args = args;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--config" => parsed.config = value(&mut args, "--config")?,
                "--signing-key" => parsed.signing_key = Some(value(&mut args, "--signing-key")?),
                _ => return Err(ArgsError::Unknown { argument }),
            }
        }
        Ok(parsed)
    }
}

fn value(
    args: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<PathBuf, ArgsError> {
    args.next()
        .map(PathBuf::from)
        .ok_or(ArgsError::MissingValue { flag })
}
