//! The flags, which are the whole operator interface. Nothing is defaulted, so
//! every refusal here is a run that would otherwise have synced from somewhere
//! nobody named.

use std::path::PathBuf;

use metsuke_fetch::cli::{Args, ArgsError, Command, Invocation};
use metsuke_fetch::select::Selection;
use metsuke_wire::envelope::{AgentId, PoolId, SigningKey};
use metsuke_wire::key::{KEY_PREFIX, Kind};

/// How every command reaches the archive, as flag and value pairs.
const ACCESS: [[&str; 2]; 4] = [
    ["--server", "http://archive.example:8080"],
    ["--user", "developer"],
    ["--password-file", "/run/credentials/password"],
    ["--timeout-ms", "30000"],
];

fn invoked(args: &[&str]) -> Result<Invocation, ArgsError> {
    Invocation::parse(args.iter().map(|argument| argument.to_string()))
}

/// The cases below are all about a command's flags, so they read the `Args` a
/// run gets and treat anything else here as the test being wrong.
fn parsed(args: &[&str]) -> Result<Args, ArgsError> {
    invoked(args).map(|invocation| match invocation {
        Invocation::Run(args) => *args,
        asked => panic!("these arguments name a command, not {asked:?}"),
    })
}

/// `command` with every access flag, plus whatever the case is about.
fn parsed_command(command: &str, extra: &[&str]) -> Result<Args, ArgsError> {
    let mut args = vec![command];
    args.extend(ACCESS.iter().flatten());
    args.extend(extra);
    parsed(&args)
}

#[test]
fn a_sync_names_its_state_file_and_its_download_directory() {
    let args = parsed_command(
        "sync",
        &[
            "--state",
            "/var/lib/fetch/cursor.json",
            "--into",
            "/srv/archive",
        ],
    )
    .expect("every flag is given");

    assert_eq!(
        args.command,
        Command::Sync {
            state: PathBuf::from("/var/lib/fetch/cursor.json"),
            into: PathBuf::from("/srv/archive"),
        }
    );
    assert_eq!(args.access.server, "http://archive.example:8080");
    assert_eq!(args.access.user, "developer");
    assert_eq!(
        args.access.password_file,
        PathBuf::from("/run/credentials/password")
    );
    assert_eq!(args.access.timeout_ms.get(), 30_000);
    assert_eq!(args.selection, Selection::default());
}

#[test]
fn a_prefix_is_optional_and_taken_as_given() {
    let args =
        parsed_command("list", &["--prefix", "v1/2026-08-27/"]).expect("a listing needs no state");

    assert_eq!(args.command, Command::List);
    assert_eq!(args.prefix, "v1/2026-08-27/");
}

#[test]
fn an_absent_or_empty_prefix_is_the_archives_own() {
    for given in [vec![], vec!["--prefix", ""]] {
        let args = parsed_command("list", &given).expect("a prefix is optional");

        assert_eq!(args.prefix, KEY_PREFIX, "{given:?}");
    }
}

#[test]
fn the_three_key_filters_are_read_off_the_flags() {
    let pool = PoolId::from_cold_key(&SigningKey::from_bytes(&[7u8; 32]).verifying_key());

    let args = parsed_command(
        "list",
        &[
            "--pool",
            &pool.to_bech32(),
            "--agent",
            "relay-1",
            "--kind",
            "logs",
        ],
    )
    .expect("every filter parses");

    assert_eq!(
        args.selection,
        Selection {
            pool: Some(pool),
            agent: Some(AgentId::parse("relay-1").expect("a slug")),
            kind: Some(Kind::Logs),
        }
    );
}

/// A filter that does not parse would select nothing at the archive, which
/// reads as an empty corpus rather than as the typo it is.
#[test]
fn a_filter_that_is_not_one_is_refused_naming_the_flag() {
    for (flag, value) in [
        ("--pool", "pool1nope"),
        ("--agent", "Relay_1"),
        ("--kind", "traces"),
    ] {
        let error = parsed_command("list", &[flag, value])
            .expect_err("a filter is refused where it is written");

        assert!(
            matches!(&error, ArgsError::NotAFilter { flag: named, .. } if *named == flag),
            "{flag} {value}: {error}"
        );
    }
}

#[test]
fn a_sync_without_a_state_file_is_refused() {
    let error = parsed_command("sync", &["--into", "/srv/archive"])
        .expect_err("a sync with no cursor would restart every run");

    assert!(
        matches!(
            error,
            ArgsError::Missing {
                flag: "--state",
                ..
            }
        ),
        "got: {error}"
    );
}

/// A listing writes nothing, so a flag about where downloads go is a
/// misunderstanding rather than a value to ignore.
#[test]
fn a_listing_given_a_download_directory_is_refused() {
    let error =
        parsed_command("list", &["--into", "/srv/archive"]).expect_err("list downloads nothing");

    assert!(
        matches!(
            error,
            ArgsError::NotForCommand {
                command: "list",
                flag: "--into"
            }
        ),
        "got: {error}"
    );
}

#[test]
fn every_access_flag_is_required() {
    for flag in ACCESS.map(|[flag, _]| flag) {
        let mut args = vec!["list"];
        args.extend(ACCESS.iter().filter(|[name, _]| *name != flag).flatten());
        let error = parsed(&args).expect_err("nothing here has a default");

        assert!(
            matches!(&error, ArgsError::Missing { flag: missing, .. } if *missing == flag),
            "{flag}: {error}"
        );
    }
}

#[test]
fn a_timeout_that_is_not_a_positive_number_of_milliseconds_is_refused() {
    for value in ["0", "-1", "30s", ""] {
        let mut args = vec!["list"];
        args.extend(
            ACCESS
                .iter()
                .filter(|[name, _]| *name != "--timeout-ms")
                .flatten(),
        );
        args.extend(["--timeout-ms", value]);
        let error = parsed(&args).expect_err("a deadline is a positive count of milliseconds");

        assert!(
            matches!(&error, ArgsError::NotADuration { .. }),
            "{value:?}: {error}"
        );
    }
}

#[test]
fn an_unknown_command_and_an_unknown_flag_are_both_refused() {
    assert!(matches!(
        parsed(&["fetch"]),
        Err(ArgsError::UnknownCommand { .. })
    ));
    assert!(matches!(
        parsed_command("list", &["--everything", "yes"]),
        Err(ArgsError::Unknown { .. })
    ));
    assert!(matches!(parsed(&[]), Err(ArgsError::NoCommand)));
}

#[test]
fn a_flag_without_a_value_is_refused() {
    let error = parsed(&["list", "--server"]).expect_err("a flag takes a value");

    assert!(
        matches!(error, ArgsError::MissingValue { flag: "--server" }),
        "got: {error}"
    );
}

#[test]
fn help_and_version_are_answered_without_an_endpoint_or_a_credential() {
    assert!(matches!(invoked(&["--version"]), Ok(Invocation::Version)));
    assert!(matches!(invoked(&["--help"]), Ok(Invocation::Help)));
    assert!(matches!(invoked(&["-h"]), Ok(Invocation::Help)));
}

/// Where an operator writes them: after the command, and without the access
/// flags every command otherwise needs.
#[test]
fn help_and_version_are_answered_wherever_they_are_written() {
    assert!(matches!(invoked(&["list", "--help"]), Ok(Invocation::Help)));
    assert!(matches!(
        invoked(&["sync", "--prefix", "v1/", "--version"]),
        Ok(Invocation::Version)
    ));
}
