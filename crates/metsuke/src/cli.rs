//! CLI arguments, hand-parsed: two flags don't earn a parser dependency on
//! a binary whose attack surface is the product.

use std::path::PathBuf;

/// Shipped config location; `--config` overrides it.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/metsuke/config.toml";

/// What `--version` answers. Digits and dots only, or the update nudge stops
/// working: docs/releasing.md, Checking the nudge works.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const USAGE: &str = "metsuke reports a cardano-node's telemetry for a MusashiNet pool.

usage:
  metsuke [--config <path>] [--signing-key <path>]
  cardano-node run ... | metsuke [--config <path>] [--signing-key <path>]
  metsuke --help | --version

flags:
  --config <path>        the agent's configuration;
                         /etc/metsuke/config.toml by default
  --signing-key <path>   the pool's cold or Leios signing key, overriding the
                         config's signing_key; this is what the unit's
                         LoadCredential hands the agent

The agent scrapes the loopback Prometheus endpoint the config names, spools
what it read, and uploads signed submissions to the config's upload_url. It
runs until stopped. Every limit, cadence and path other than these two flags is
configuration: contrib/config.example.toml is the annotated example.

Trace lines are collected only where the config has a [log] section, and its
source says where from. \"journald\" reads the node's unit and needs the agent
in the systemd-journal group. \"pipe\" is the second usage above: the node's
stdout arrives on this agent's stdin, and every line is written through to its
own stdout unchanged, so the agent sits between the node and whatever collected
its output before. docs/adr/0010 is what each costs.";

/// Where a refusal sends the operator, rather than printing all of `USAGE` on
/// top of the error.
pub const HELP_HINT: &str = "try metsuke --help";

#[derive(Debug, PartialEq)]
pub struct Args {
    pub config: PathBuf,
    /// Overrides the config's `signing_key`. How it interacts with
    /// LoadCredential is documented in contrib/config.example.toml.
    pub signing_key: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ArgsError {
    #[error("unknown argument {argument:?}\n{HELP_HINT}")]
    Unknown { argument: String },
    #[error("{flag} needs a <path> value\n{HELP_HINT}")]
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
