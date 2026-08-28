//! The binary as an operator runs it: one command, the keys on stdout, and an
//! exit code a script can act on.

use std::process::Command;

mod support;
use support::{PASSWORD, Server, USER};

/// The tool with a password file the test wrote, against `server`.
fn run(server: &Server, dir: &std::path::Path, extra: &[&str]) -> std::process::Output {
    let password_file = dir.join("password");
    std::fs::write(&password_file, format!("{PASSWORD}\n")).expect("the password file writes");
    Command::new(env!("CARGO_BIN_EXE_metsuke-fetch"))
        .args(extra)
        .args([
            "--server",
            &server.url,
            "--user",
            USER,
            "--password-file",
            &password_file.display().to_string(),
            "--timeout-ms",
            "10000",
        ])
        .output()
        .expect("the binary runs")
}

#[test]
fn a_sync_prints_the_keys_it_wrote_and_exits_zero() {
    let server = Server::with_objects(2, 1);
    let dir = tempfile::tempdir().expect("a temp dir");
    let into = dir.path().join("objects");

    let output = run(
        &server,
        dir.path(),
        &[
            "sync",
            "--state",
            &dir.path().join("cursor.json").display().to_string(),
            "--into",
            &into.display().to_string(),
        ],
    );

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .collect::<Vec<_>>(),
        server.keys()
    );
    for key in server.keys() {
        assert!(into.join(&key).is_file(), "{key} did not land");
    }
}

#[test]
fn an_empty_password_file_is_refused_by_name() {
    let server = Server::with_objects(1, 100);
    let dir = tempfile::tempdir().expect("a temp dir");
    let password_file = dir.path().join("password");
    std::fs::write(&password_file, "\n").expect("the password file writes");

    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-fetch"))
        .args([
            "list",
            "--server",
            &server.url,
            "--user",
            USER,
            "--password-file",
            &password_file.display().to_string(),
            "--timeout-ms",
            "10000",
        ])
        .output()
        .expect("the binary runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&password_file.display().to_string()),
        "got: {stderr}"
    );
}

/// The version a Developer reads off a build is the one the crate shipped, so
/// this compares the binary's answer with the manifest's value rather than a
/// string written here.
#[test]
fn version_is_printed_on_its_own_and_names_the_crates_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-fetch"))
        .arg("--version")
        .output()
        .expect("the binary runs");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        env!("CARGO_PKG_VERSION")
    );
}

/// Asked for, so it is the answer: stdout, exit zero, and the usage the
/// parser refuses with.
#[test]
fn help_is_printed_on_stdout_and_exits_zero() {
    for flag in ["--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_metsuke-fetch"))
            .arg(flag)
            .output()
            .expect("the binary runs");

        assert!(
            output.status.success(),
            "{flag}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim_end(),
            metsuke_fetch::cli::USAGE
        );
    }
}

#[test]
fn a_sync_names_the_build_that_wrote_the_download() {
    let server = Server::with_objects(1, 1);
    let dir = tempfile::tempdir().expect("a temp dir");

    let output = run(
        &server,
        dir.path(),
        &[
            "sync",
            "--state",
            &dir.path().join("cursor.json").display().to_string(),
            "--into",
            &dir.path().join("objects").display().to_string(),
        ],
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(env!("CARGO_PKG_VERSION")), "got: {stderr}");
}

/// The run that most needs a build named is the one that failed, so the line
/// comes before anything that can stop the run.
#[test]
fn a_run_that_stops_on_its_password_file_still_names_the_build() {
    let dir = tempfile::tempdir().expect("a temp dir");

    let output = Command::new(env!("CARGO_BIN_EXE_metsuke-fetch"))
        .args([
            "list",
            "--server",
            "http://archive.example:8080",
            "--user",
            USER,
            "--password-file",
            &dir.path().join("absent").display().to_string(),
            "--timeout-ms",
            "10000",
        ])
        .output()
        .expect("the binary runs");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains(env!("CARGO_PKG_VERSION")), "got: {stderr}");
}
