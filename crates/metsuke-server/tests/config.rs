//! The config file has no defaults by design, so what it must prove is that
//! an absent limit and a misspelled one both fail to load rather than run the
//! server on a value nobody set.

use metsuke_server::config::{ArchiveConfig, ServerConfig};

mod support;
use support::{pool_of, test_key};

fn complete() -> String {
    format!(
        r#"
listen = "127.0.0.1:8080"
counters_path = "/var/lib/metsuke-server/counters.sqlite"

[archive]
kind = "s3"
bucket = "cardano-playground-metsuke"
region = "eu-central-1"
endpoint = "https://s3.eu-central-1.amazonaws.com"
request_timeout_secs = 30
put_retries = 1

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
    let ArchiveConfig::S3(s3) = config.archive else {
        panic!("the config named an S3 archive");
    };
    assert_eq!(s3.bucket, "cardano-playground-metsuke");
    assert_eq!(s3.put_retries, 1);
}

/// The other archive kind: a config with no bucket in it must load.
#[test]
fn a_filesystem_archive_loads() {
    let text = format!(
        r#"
listen = "127.0.0.1:8080"
counters_path = "/var/lib/metsuke-server/counters.sqlite"

[archive]
kind = "filesystem"
root = "/var/lib/metsuke-server/archive"

[ingest]
allowlist = ["{pool}"]
max_body_bytes = 1048576
max_decompressed_bytes = 4194304
rate_limit_uploads = 24
rate_limit_window_secs = 3600
max_timestamp_skew_secs = 300
"#,
        pool = pool_of(&test_key()).to_bech32(),
    );
    let config = ServerConfig::from_toml(&text).unwrap();
    let ArchiveConfig::Filesystem { root } = config.archive else {
        panic!("the config named a filesystem archive");
    };
    assert_eq!(
        root,
        std::path::Path::new("/var/lib/metsuke-server/archive")
    );
}

/// An archive with no `kind` cannot be guessed at: writing to a filesystem
/// root when the operator meant a bucket would archive nothing recoverable.
#[test]
fn an_archive_without_a_kind_is_refused() {
    let without: String = complete()
        .lines()
        .filter(|line| !line.starts_with("kind ="))
        .collect::<Vec<_>>()
        .join("\n");
    let error = ServerConfig::from_toml(&without).unwrap_err().to_string();
    assert!(error.contains("kind"), "got: {error}");
}

#[test]
fn every_field_is_required() {
    for field in [
        "listen",
        "counters_path",
        "bucket",
        "region",
        "endpoint",
        "request_timeout_secs",
        "put_retries",
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
