//! The config file has no defaults by design, so what it must prove is that an
//! absent limit, a zero one and a misspelled one all fail to load rather than
//! run the server on a value nobody set.

use metsuke_server::config::{ArchiveConfig, ServerConfig};

mod support;
use support::{example_config as example, pool_of, server_toml, test_key};

/// The example is the complete config every test here mutates, so a field the
/// server grows must reach the file an operator copies from before this suite
/// passes again.
#[test]
fn the_shipped_example_config_loads() {
    let config = ServerConfig::from_toml(&example()).unwrap();
    assert_eq!(config.listen, "127.0.0.1:8080");
    assert!(config.ingest.allowlist.contains_key(&pool_of(&test_key())));
    assert_eq!(config.ingest.max_timestamp_skew_secs.get(), 300);
    let ArchiveConfig::S3(s3) = config.archive else {
        panic!("the example names an S3 archive");
    };
    assert_eq!(s3.bucket, "cardano-playground-metsuke");
    assert_eq!(s3.put_retries, 1);
    assert_eq!(
        s3.endpoint.host_str(),
        Some("s3.eu-central-1.amazonaws.com")
    );
}

/// The other archive kind, and a config leaving `[applications]` out.
#[test]
fn a_filesystem_archive_without_an_applications_section_loads() {
    let dir = tempfile::tempdir().unwrap();
    let text = server_toml(dir.path(), &[pool_of(&test_key())]).render();
    let config = ServerConfig::from_toml(&text).unwrap();
    let ArchiveConfig::Filesystem { root } = config.archive else {
        panic!("the config named a filesystem archive");
    };
    assert_eq!(root, dir.path().join("archive"));
    assert!(config.applications.is_none());
}

/// `example()` with the line that sets `field` replaced by `replacement`, or
/// dropped when there is none. The setting line is matched on its own, so the
/// commented-out `[archive]` block and the prose above it are left alone.
fn rewriting(field: &str, replacement: Option<String>) -> String {
    let setting = format!("{field} =");
    example()
        .lines()
        .filter_map(|line| match line.starts_with(&setting) {
            true => replacement.clone(),
            false => Some(line.to_string()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `example()` with one field's line dropped.
fn without(field: &str) -> String {
    rewriting(field, None)
}

/// `example()` with one field set to a value that must not load.
fn with(field: &str, value: &str) -> String {
    rewriting(field, Some(format!("{field} = {value}")))
}

/// An archive with no `kind` cannot be guessed at: writing to a filesystem
/// root when the operator meant a bucket would archive nothing recoverable.
#[test]
fn an_archive_without_a_kind_is_refused() {
    let error = ServerConfig::from_toml(&without("kind"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("kind"), "got: {error}");
}

/// The fields typed `NonZero`, so zero and absence are the same refusal.
const NONZERO_FIELDS: [&str; 13] = [
    "query_timeout_secs",
    "list_max_rows",
    "resolution_ttl_secs",
    "max_registrations",
    "request_timeout_secs",
    "signature_validity_secs",
    "put_retry_backoff_ms",
    "list_max_pages",
    "max_body_bytes",
    "max_decompressed_bytes",
    "rate_limit_uploads",
    "rate_limit_window_secs",
    "max_timestamp_skew_secs",
];

/// The rest, where only absence is a mistake. Together with `NONZERO_FIELDS`
/// this is every field the server reads, so a new one joins exactly one list.
const OTHER_FIELDS: [&str; 14] = [
    "listen",
    "user",
    "index_path",
    "bucket",
    "region",
    "endpoint",
    "put_retries",
    "allowlist",
    "applications_csv",
    "socket_dir",
    "dbname",
    "role",
    "password_file",
    "shelley_genesis_path",
];

#[test]
fn every_field_is_required() {
    for field in NONZERO_FIELDS.iter().chain(&OTHER_FIELDS) {
        let error = ServerConfig::from_toml(&without(field))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(field),
            "config without {field} must fail naming it, got: {error}"
        );
    }
}

#[test]
fn a_path_that_is_not_absolute_is_refused() {
    for field in [
        "socket_dir",
        "applications_csv",
        "password_file",
        "shelley_genesis_path",
    ] {
        let error = ServerConfig::from_toml(&with(field, "\"relative\""))
            .unwrap_err()
            .to_string();
        assert!(error.contains("absolute"), "{field}, got: {error}");
    }
}

#[test]
fn a_misspelled_field_is_refused() {
    let typo = example().replace("max_body_bytes", "max_bodybytes");
    let error = ServerConfig::from_toml(&typo).unwrap_err().to_string();
    assert!(error.contains("max_bodybytes"), "got: {error}");
}

#[test]
fn a_pool_id_that_is_not_bech32_is_refused() {
    let broken = example().replace(&pool_of(&test_key()).to_bech32(), "pool1nope");
    assert!(ServerConfig::from_toml(&broken).is_err());
}
/// Zero is the same deployment mistake as an absent value, and it is the more
/// dangerous one: `request_timeout_secs = 0` builds an agent that fails every
/// PUT, so every upload becomes a 503 nobody can trace to this file.
#[test]
fn a_field_where_zero_means_nothing_is_refused() {
    for field in NONZERO_FIELDS {
        let error = ServerConfig::from_toml(&with(field, "0"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("nonzero"),
            "{field} = 0 must fail, got: {error}"
        );
    }
}

/// Where the refusal points. An `[ingest]` field is named at its own line; an
/// `[archive]` one is not, because serde's internally-tagged enum buffers the
/// table and drops the span with it (ticket metsuke-zzs).
#[test]
fn a_refused_value_is_pointed_at_by_its_own_line() {
    let error = ServerConfig::from_toml(&with("max_body_bytes", "0"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("max_body_bytes = 0"), "got: {error}");

    let error = ServerConfig::from_toml(&with("request_timeout_secs", "0"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("[archive]"), "got: {error}");
}

/// The `NonZero` exception, as `S3Config::put_retries` states it.
#[test]
fn put_retries_may_be_zero() {
    let none = example().replace("put_retries = 1", "put_retries = 0");
    let ArchiveConfig::S3(s3) = ServerConfig::from_toml(&none).unwrap().archive else {
        panic!("the example names an S3 archive");
    };
    assert_eq!(s3.put_retries, 0);
}

/// A bad endpoint is a config error quoting what was written, not a startup
/// failure from somewhere inside the archive.
#[test]
fn an_endpoint_that_is_not_a_url_is_refused_at_load() {
    let error = ServerConfig::from_toml(&with("endpoint", "\"not a url\""))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a url"), "got: {error}");
}
