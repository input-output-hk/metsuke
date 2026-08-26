//! The application code as the allowlist holds it: what parses, and that the
//! pairs an operator writes are the ones the server's own parser reads back.

use metsuke_server::applications::ApplicationCode;
use metsuke_server::config::ServerConfig;

mod support;
use support::{allowlist, allowlist_toml, example_config, other_key, pool_of, test_key};

#[test]
fn an_empty_code_is_refused() {
    assert!(ApplicationCode::parse("").is_err());
    assert!(ApplicationCode::parse("  ").is_err());
}

#[test]
fn a_code_is_matched_without_its_surrounding_whitespace() {
    assert_eq!(
        ApplicationCode::parse(" MUSA-1 ").unwrap().as_str(),
        "MUSA-1"
    );
}

#[test]
fn a_code_outside_the_identifier_alphabet_is_refused() {
    for text in [
        "MUSA 1", "MUSA\"1", "MUSA\n1", "MUSA/1", "MUSA=1", "MUSA[1]",
    ] {
        assert!(
            ApplicationCode::parse(text).is_err(),
            "{text:?} must not parse as a code"
        );
    }
}

/// Acceptance: the pairs an allowlist is written as are the value the server's
/// own parser reads for `ingest.allowlist`.
#[test]
fn the_written_pairs_load_as_the_servers_allowlist() {
    let allowed = allowlist(&[pool_of(&test_key()), pool_of(&other_key())]);
    let config: String = example_config()
        .lines()
        .map(|line| match line.starts_with("allowlist = ") {
            true => format!("allowlist = {}", allowlist_toml(&allowed)),
            false => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let loaded = ServerConfig::from_toml(&config).expect("the example config parses");

    assert_eq!(loaded.ingest.allowlist, allowed);
}
