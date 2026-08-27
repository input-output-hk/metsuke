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
