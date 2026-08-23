//! Agent config tests (ticket metsuke-4zo.5): TOML in, validated `Config`
//! out. Required fields fail loudly when absent; cadence and probe knobs
//! default to the shipped values the example config documents.

use metsuke::config::Config;
use metsuke_wire::envelope::{PoolId, SigningKey};

fn test_pool_id() -> PoolId {
    PoolId::from_cold_key(&SigningKey::from_bytes(&[7u8; 32]).verifying_key())
}

fn minimal_toml() -> String {
    format!(
        r#"
        pool_id = "{}"
        metrics_url = "http://127.0.0.1:12798/metrics"
        upload_url = "https://metsuke.example.org/v1/submit"
        "#,
        test_pool_id().to_bech32()
    )
}

// Defaults per the spec: sample every 5 minutes, upload every hour,
// SNTP against time.cloudflare.com.
#[test]
fn minimal_config_parses_with_shipped_defaults() {
    let config = Config::from_toml(&minimal_toml()).unwrap();
    assert_eq!(config.pool_id, test_pool_id());
    assert_eq!(config.metrics_url, "http://127.0.0.1:12798/metrics");
    assert_eq!(
        config.upload_url.as_str(),
        "https://metsuke.example.org/v1/submit"
    );
    assert_eq!(config.signing_key, None);
    assert_eq!(config.sample_interval_secs, 300);
    assert_eq!(config.upload_interval_secs, 3600);
    assert_eq!(config.sntp_servers, vec!["time.cloudflare.com:123"]);
    assert_eq!(config.sntp_timeout_secs, 5);
    assert_eq!(
        config.spool_path,
        std::path::PathBuf::from("/var/lib/metsuke/spool.sqlite")
    );
    assert_eq!(config.spool_max_samples, 100_000);
    assert_eq!(config.scrape_timeout_secs, 5);
    assert_eq!(config.scrape_max_body_bytes, 4 * 1024 * 1024);
    assert_eq!(config.upload_timeout_secs, 60);
    assert_eq!(config.upload_jitter_max_secs, 300);
    assert_eq!(config.upload_backoff_max_secs, 86_400);
    assert_eq!(config.compression_level, 0);
}

// Acceptance: sample and upload cadences are independent configuration —
// setting one leaves the other at its default.
#[test]
fn sample_and_upload_cadences_are_independent() {
    let toml = format!("{}\nsample_interval_secs = 60\n", minimal_toml());
    let config = Config::from_toml(&toml).unwrap();
    assert_eq!(config.sample_interval_secs, 60);
    assert_eq!(config.upload_interval_secs, 3600);
}

// The example config's commented defaults must be the code's defaults:
// uncommenting every `# key = value` line must parse and change nothing
// (required fields aside, which the example marks with CHANGEME).
#[test]
fn example_config_documents_the_real_defaults() {
    let example = include_str!("../../../contrib/config.example.toml");
    let uncommented: String = example
        .lines()
        .map(|line| {
            line.strip_prefix("# ")
                .filter(|rest| {
                    rest.split_once(" = ").is_some_and(|(key, _)| {
                        !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                    })
                })
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("pool1CHANGEME", &test_pool_id().to_bech32())
        // The signing_key line documents the flag interplay, not a default.
        .replace("signing_key = ", "# signing_key = ");
    let all_defaults = Config::from_toml(&uncommented).unwrap();
    let minimal = Config::from_toml(&minimal_toml()).unwrap();
    assert_eq!(
        all_defaults,
        Config {
            metrics_url: all_defaults.metrics_url.clone(),
            upload_url: all_defaults.upload_url.clone(),
            ..minimal
        }
    );
}

// Every value is required or an explicit default: a config without a
// required field must fail at startup, not run degraded.
#[test]
fn missing_required_field_fails_loudly() {
    let without_upload_url = minimal_toml()
        .lines()
        .filter(|line| !line.contains("upload_url"))
        .collect::<Vec<_>>()
        .join("\n");
    let err = Config::from_toml(&without_upload_url).unwrap_err();
    assert!(
        err.to_string().contains("upload_url"),
        "error must name the missing field, got: {err}"
    );
}

// Ticket metsuke-4zo.40.
#[test]
fn plaintext_upload_url_fails_loudly() {
    let toml = minimal_toml().replace("https://metsuke", "http://metsuke");
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains("upload_url"),
        "error must name the field, got: {err}"
    );
}

// The loopback exemption (why: `UploadUrlError::Plaintext`) is what the
// integration suite uploads through.
#[test]
fn loopback_plaintext_upload_url_parses() {
    let toml = minimal_toml().replace("https://metsuke.example.org", "http://127.0.0.1:9000");
    let config = Config::from_toml(&toml).unwrap();
    assert_eq!(
        config.upload_url.as_str(),
        "http://127.0.0.1:9000/v1/submit"
    );
}

// Dropping the scheme leaves something the URL parser rejects outright,
// which is a different operator mistake from a scheme the agent refuses.
#[test]
fn schemeless_upload_url_says_it_does_not_parse() {
    let toml = minimal_toml().replace("https://metsuke.example.org", "metsuke.example.org");
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains("does not parse as a URL"),
        "error must say it does not parse, got: {err}"
    );
}

// A path alone parses, so only the absent host makes it unusable: say that
// rather than blame the parser.
#[test]
fn path_only_upload_url_names_the_missing_host() {
    let toml = minimal_toml().replace("https://metsuke.example.org/v1/submit", "/v1/submit");
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string()
            .contains("must be an absolute URL with a host"),
        "error must name what it wanted, got: {err}"
    );
}

// A typo'd knob silently falling back to a default would hide operator
// intent; unknown fields reject instead.
#[test]
fn unknown_field_fails_loudly() {
    let toml = format!("{}\nupload_intervall_secs = 60\n", minimal_toml());
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains("upload_intervall_secs"),
        "error must name the unknown field, got: {err}"
    );
}
