//! The config file has no defaults by design, so what it must prove is that
//! an absent limit and a misspelled one both fail to load rather than run the
//! server on a value nobody set.

use metsuke_server::config::ServerConfig;

mod support;
use support::{pool_of, test_key};

fn complete() -> String {
    format!(
        r#"
listen = "127.0.0.1:8080"
counters_path = "/var/lib/metsuke-server/counters.sqlite"
archive_root = "/var/lib/metsuke-server/archive"

[ingest]
allowlist = ["{pool}"]
max_body_bytes = 1048576
max_decompressed_bytes = 4194304
rate_limit_uploads = 24
rate_limit_window_secs = 3600
max_timestamp_skew_secs = 300
"#,
        pool = pool_of(&test_key()).to_bech32(),
    )
}

#[test]
fn a_complete_config_loads() {
    let config = ServerConfig::from_toml(&complete()).unwrap();
    assert_eq!(config.listen, "127.0.0.1:8080");
    assert!(config.ingest.allowlist.contains(&pool_of(&test_key())));
    assert_eq!(config.ingest.max_timestamp_skew_secs, 300);
}

#[test]
fn every_field_is_required() {
    for field in [
        "listen",
        "counters_path",
        "archive_root",
        "allowlist",
        "max_body_bytes",
        "max_decompressed_bytes",
        "rate_limit_uploads",
        "rate_limit_window_secs",
        "max_timestamp_skew_secs",
    ] {
        let without: String = complete()
            .lines()
            .filter(|line| !line.starts_with(&format!("{field} =")))
            .collect::<Vec<_>>()
            .join("\n");
        let error = ServerConfig::from_toml(&without).unwrap_err().to_string();
        assert!(
            error.contains(field),
            "config without {field} must fail naming it, got: {error}"
        );
    }
}

#[test]
fn a_misspelled_field_is_refused() {
    let typo = complete().replace("max_body_bytes", "max_bodybytes");
    let error = ServerConfig::from_toml(&typo).unwrap_err().to_string();
    assert!(error.contains("max_bodybytes"), "got: {error}");
}

#[test]
fn a_pool_id_that_is_not_bech32_is_refused() {
    let broken = complete().replace(&pool_of(&test_key()).to_bech32(), "pool1nope");
    assert!(ServerConfig::from_toml(&broken).is_err());
}
