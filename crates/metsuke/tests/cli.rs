//! CLI parsing tests (ticket metsuke-4zo.5): two flags, hand-parsed. They are
//! `--config` and the `--signing-key` override systemd LoadCredential uses.

use std::path::PathBuf;

use metsuke::cli::Args;

fn parse(args: &[&str]) -> Result<Args, metsuke::cli::ArgsError> {
    Args::parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn no_flags_yield_the_shipped_config_path_and_no_key_override() {
    let args = parse(&[]).unwrap();
    assert_eq!(args.config, PathBuf::from("/etc/metsuke/config.toml"));
    assert_eq!(args.signing_key, None);
}

#[test]
fn both_flags_parse() {
    let args = parse(&["--config", "/tmp/c.toml", "--signing-key", "/tmp/k.skey"]).unwrap();
    assert_eq!(args.config, PathBuf::from("/tmp/c.toml"));
    assert_eq!(args.signing_key, Some(PathBuf::from("/tmp/k.skey")));
}

// A mistyped flag must fail loudly, not run with defaults the operator
// believed they overrode.
#[test]
fn unknown_flag_fails_loudly() {
    let err = parse(&["--siging-key", "/tmp/k.skey"]).unwrap_err();
    assert!(err.to_string().contains("--siging-key"));
}

#[test]
fn flag_missing_its_value_fails_loudly() {
    let err = parse(&["--signing-key"]).unwrap_err();
    assert!(err.to_string().contains("--signing-key"));
}
