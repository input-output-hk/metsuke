//! Agent config tests (ticket metsuke-4zo.5): TOML in, validated `Config`
//! out. Required fields fail loudly when absent; cadence and probe knobs
//! default to the shipped values the example config documents.

use metsuke::config::{Config, LogConfig, LogSource};
use metsuke::logsource::JournalConfig;
use metsuke_wire::envelope::{PoolId, SigningKey};

fn journal(log: &LogConfig) -> &JournalConfig {
    match &log.source {
        LogSource::Journald(journal) => journal,
        other => panic!("expected a journald source, got {other:?}"),
    }
}

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

/// A `[log]` section holding only what has no default: the two paths that
/// describe this host and nothing about which lines to ship.
fn minimal_log_section() -> String {
    "[log]\nsource = \"journald\"\njournal_unit = \"cardano-node\"\njournalctl_path = \"/usr/bin/journalctl\"".to_string()
}

// Defaults per the spec: sample every 5 minutes, upload every hour,
// SNTP against time.cloudflare.com.
#[test]
fn minimal_config_parses_with_shipped_defaults() {
    let config = Config::from_toml(&minimal_toml()).unwrap();
    assert_eq!(config.pool_id, test_pool_id());
    assert_eq!(
        config.metrics_url.as_str(),
        "http://127.0.0.1:12798/metrics"
    );
    assert_eq!(
        config.upload_url.as_str(),
        "https://metsuke.example.org/v1/submit"
    );
    assert_eq!(config.signing_key, None);
    assert_eq!(config.agent_id, None);
    assert_eq!(config.sample_interval_secs, 300);
    assert_eq!(config.upload_interval_secs, 3600);
    assert_eq!(config.sntp_servers, vec!["time.cloudflare.com:123"]);
    assert_eq!(config.sntp_timeout_secs, 5);
    assert_eq!(
        config.spool_path,
        std::path::PathBuf::from("/var/lib/metsuke/spool.sqlite")
    );
    assert_eq!(config.spool_max_bytes, 32 * 1024 * 1024);
    assert_eq!(config.spool_busy_timeout_secs, 5);
    assert_eq!(config.upload_batch_max_bytes, 4 * 1024 * 1024);
    assert_eq!(config.scrape_timeout_secs, 5);
    assert_eq!(config.scrape_max_body_bytes, 4 * 1024 * 1024);
    assert_eq!(config.upload_timeout_secs, 60);
    assert_eq!(config.upload_jitter_max_secs, 300);
    assert_eq!(config.upload_backoff_max_secs, 86_400);
    assert_eq!(config.compression_level, 0);
    assert_eq!(config.log, None);
}

// ADR 0010: trace collection is what the systemd-journal grant is for, so an
// agent nobody configured for it holds the privileges ADR 0007 allowed and
// starts no journalctl.
#[test]
fn trace_collection_is_absent_until_it_is_configured() {
    assert_eq!(Config::from_toml(&minimal_toml()).unwrap().log, None);
}

// Only the host's own two paths have no default: everything else about which
// lines to ship is shipped configuration.
#[test]
fn a_log_section_naming_only_the_host_s_paths_takes_the_shipped_defaults() {
    let toml = format!("{}\n{}\n", minimal_toml(), minimal_log_section());
    let log = Config::from_toml(&toml).unwrap().log.unwrap();
    assert_eq!(journal(&log).journal_unit, "cardano-node");
    assert_eq!(log.namespace_roots, ["Consensus.", "ChainDB.", "Forge."]);
    assert_eq!(
        log.namespaces,
        [
            "Consensus.Leios",
            "ChainDB.AddBlockEvent.AddedToCurrentChain",
            "Forge.Loop.AdoptedBlock",
        ]
    );
    assert_eq!(log.log_max_bytes, 256 * 1024 * 1024);
    assert_eq!(log.respawn_backoff_secs, 30);
}

// Neither path is guessable, so a section missing one has to say which.
#[test]
fn a_log_section_without_one_of_the_host_s_paths_fails_loudly() {
    for missing in ["journal_unit", "journalctl_path"] {
        let section: String = minimal_log_section()
            .lines()
            .filter(|line| !line.starts_with(missing))
            .collect::<Vec<_>>()
            .join("\n");
        let err = Config::from_toml(&format!("{}\n{section}\n", minimal_toml())).unwrap_err();
        assert!(
            err.to_string().contains(missing),
            "error must name the missing field, got: {err}"
        );
    }
}

#[test]
fn an_unknown_log_key_fails_loudly() {
    let toml = format!(
        "{}\n{}\nmin_severities = \"Error\"\n",
        minimal_toml(),
        minimal_log_section()
    );
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains("min_severities"),
        "error must name the unknown field, got: {err}"
    );
}

// The retired severity floor. An operator carrying it over from an earlier
// config is told the key is gone, rather than having it read and ignored while
// selection quietly stops honouring it.
#[test]
fn the_retired_min_severity_key_fails_loudly() {
    let toml = format!(
        "{}\n{}\nmin_severity = \"Warning\"\n",
        minimal_toml(),
        minimal_log_section()
    );
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains("min_severity"),
        "error must name the retired field, got: {err}"
    );
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

const EXAMPLE: &str = include_str!("../../../contrib/config.example.toml");

/// Where the example's optional `[log]` section starts. Everything after it
/// belongs to that table once uncommented, so a test about the top-level
/// defaults has to stop here.
const LOG_SECTION: &str = "# [log]";

/// The example as it reads with every documented value in force: `# key =
/// value` and `# [table]` lines lose their comment, prose keeps it.
fn uncomment(example: &str) -> String {
    example
        .lines()
        .map(|line| {
            line.strip_prefix("# ")
                .filter(|rest| {
                    rest.starts_with('[')
                        || rest.split_once(" = ").is_some_and(|(key, _)| {
                            !key.is_empty()
                                && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                        })
                })
                .unwrap_or(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("pool1CHANGEME", &test_pool_id().to_bech32())
        // The signing_key line documents the flag interplay, not a default.
        .replace("signing_key = ", "# signing_key = ")
}

// The example config's commented defaults must be the code's defaults:
// uncommenting every `# key = value` line must parse and change nothing
// (required fields aside, which the example marks with CHANGEME).
#[test]
fn example_config_documents_the_real_defaults() {
    let (before_log, _) = EXAMPLE
        .split_once(LOG_SECTION)
        .expect("the example documents the optional [log] section");
    let all_defaults = Config::from_toml(&uncomment(before_log)).unwrap();
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

// The same for the optional section, which the test above has to stop short
// of: its values are the shipped ones, and only the two host paths are choices.
#[test]
fn the_example_log_section_documents_the_real_defaults() {
    let documented = Config::from_toml(&uncomment(EXAMPLE))
        .unwrap()
        .log
        .expect("the example documents a [log] section");
    let shipped = Config::from_toml(&format!(
        "{}\n[log]\nsource = \"journald\"\njournal_unit = \"{}\"\njournalctl_path = \"{}\"\n",
        minimal_toml(),
        journal(&documented).journal_unit,
        journal(&documented).journalctl_path.display(),
    ))
    .unwrap()
    .log
    .unwrap();
    assert_eq!(documented, shipped);
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

/// The rule both rejections must name, so neither passes on a `NotAbsolute`
/// that happens to mention the field.
const NOT_LOOPBACK: &str = "metrics_url must be http or https to a loopback address";

// Ticket metsuke-4zo.42; why loopback only: `endpoint::MetricsUrl`.
#[test]
fn off_host_metrics_url_fails_loudly() {
    let toml = minimal_toml().replace("127.0.0.1:12798", "10.0.0.5:12798");
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains(NOT_LOOPBACK),
        "error must name the rule, got: {err}"
    );
}

// `localhost` is a name, not an address (why that matters: `MetricsUrl`).
#[test]
fn metrics_url_naming_localhost_fails_loudly() {
    let toml = minimal_toml().replace("127.0.0.1:12798", "localhost:12798");
    let err = Config::from_toml(&toml).unwrap_err();
    assert!(
        err.to_string().contains(NOT_LOOPBACK),
        "error must name the rule, got: {err}"
    );
}

#[test]
fn ipv6_loopback_metrics_url_parses() {
    let toml = minimal_toml().replace("127.0.0.1:12798", "[::1]:12798");
    let config = Config::from_toml(&toml).unwrap();
    assert_eq!(config.metrics_url.as_str(), "http://[::1]:12798/metrics");
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
