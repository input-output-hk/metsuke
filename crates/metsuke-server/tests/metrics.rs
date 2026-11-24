//! The exposition, read back the way a scraper reads it: through a parser that
//! did not write it (`metsuke::scrape::parse`, the agent's, written for
//! cardano-node's endpoint). Submissions go through `http::answer`, which is
//! where every refusal of the route is counted.

use metsuke::scrape::parse;
use metsuke_server::archive::FilesystemArchive;
use metsuke_server::authority::{Attributed, AttributionError};
use metsuke_server::config::IngestConfig;
use metsuke_server::developer::Developer;
use metsuke_server::http::{self, Method, Request, SUBMIT_PATH};
use metsuke_server::intake::Intake;
use metsuke_server::metrics::Refusal;
use metsuke_wire::envelope::{Envelope, Metric, SigningKey};
use metsuke_wire::hex;
use time::OffsetDateTime;

mod support;
use support::{
    developer_accounts, developer_config, envelope_at, envelope_for, intake_with,
    lines_envelope_at, other_key, permissive_config, pool_of, seal, skew_of, test_key, test_now,
    test_pages, trace_line,
};

/// What a submission is answered by, as `serve` hands it over.
struct Server {
    intake: Intake<FilesystemArchive>,
    developer: Developer,
    pages: http::Pages,
    _dir: tempfile::TempDir,
}

impl Server {
    fn over(config: IngestConfig) -> Server {
        let (intake, dir) = intake_with(config);
        Server {
            developer: Developer::new(&developer_config(dir.path()), developer_accounts()),
            pages: http::Pages::from(test_pages()),
            intake,
            _dir: dir,
        }
    }

    /// The submission sealed by `key`, its status and the body's length.
    fn submit(&self, key: &SigningKey, envelope: &Envelope) -> (u16, usize) {
        let (body, attestation) = seal(key, envelope);
        let length = body.len();
        let submission = Attributed::decode(
            Some(&hex::encode(&attestation.key_bytes())),
            Some(&hex::encode(&attestation.signature_bytes())),
            None,
        );
        (self.post(submission, body), length)
    }

    fn post(&self, submission: Result<Attributed, AttributionError>, body: Vec<u8>) -> u16 {
        let request = Request {
            method: Method::Post,
            target: SUBMIT_PATH.to_string(),
            submission,
            authorization: None,
            body,
        };
        http::answer(&self.intake, &self.developer, &self.pages, request).status
    }

    /// Every sample the exposition states. A line the parser could not read
    /// is a failure here, whatever the samples it did read say.
    fn scraped(&self) -> Vec<Metric> {
        let parsed = parse(&self.intake.metrics().render());
        assert!(
            parsed.refused.is_empty(),
            "unreadable: {:?}",
            parsed.refused
        );
        parsed.metrics
    }
}

/// The one sample of `name` carrying exactly `labels`.
fn value(samples: &[Metric], name: &str, labels: &[(&str, &str)]) -> f64 {
    let found: Vec<&Metric> = samples
        .iter()
        .filter(|metric| {
            metric.name == name
                && metric.labels.len() == labels.len()
                && labels.iter().all(|(label, text)| {
                    metric.labels.get(*label).map(String::as_str) == Some(text)
                })
        })
        .collect();
    match found.as_slice() {
        [metric] => metric.value.as_f64().expect("a sample value is a number"),
        other => panic!("want one {name}{labels:?}, found {other:?}"),
    }
}

fn of(samples: &[Metric], name: &str) -> Vec<Metric> {
    samples
        .iter()
        .filter(|metric| metric.name == name)
        .cloned()
        .collect()
}

/// Every pool on the allowlist has its series from the start, at zero, and
/// so does every reason: a series that first appears at one is a submission
/// `increase` never sees.
#[test]
fn every_allowlisted_pool_and_reason_reads_zero_before_anything_arrives() {
    let pools = [pool_of(&test_key()), pool_of(&other_key())];
    let samples = Server::over(permissive_config(&pools)).scraped();
    for pool in pools {
        let pool = pool.to_bech32();
        for kind in ["metrics", "logs"] {
            let labels = [("pool", pool.as_str()), ("kind", kind)];
            for name in [
                "metsuke_submissions_accepted_total",
                "metsuke_submission_bytes_total",
            ] {
                assert_eq!(value(&samples, name, &labels), 0.0, "{name}");
            }
        }
    }
    for refusal in Refusal::ALL {
        let labels = [("reason", refusal.label())];
        assert_eq!(
            value(&samples, "metsuke_submissions_refused_total", &labels),
            0.0
        );
    }
    assert!(of(&samples, "metsuke_agent_last_accepted_timestamp_seconds").is_empty());
}

#[test]
fn an_accept_counts_its_kind_its_bytes_and_when_its_version_landed() {
    let key = test_key();
    let pool = pool_of(&key).to_bech32();
    let server = Server::over(permissive_config(&[pool_of(&key)]));
    let before = OffsetDateTime::now_utc().unix_timestamp() as f64;
    let (status, scrape_bytes) = server.submit(&key, &envelope_for(&key, 1));
    assert_eq!(status, 200);
    let lines = lines_envelope_at(&key, 2, test_now(), vec![trace_line("{\"at\":1}")]);
    let (status, line_bytes) = server.submit(&key, &lines);
    assert_eq!(status, 200);
    let samples = server.scraped();
    for (kind, bytes) in [("metrics", scrape_bytes), ("logs", line_bytes)] {
        let labels = [("pool", pool.as_str()), ("kind", kind)];
        assert_eq!(
            value(&samples, "metsuke_submissions_accepted_total", &labels),
            1.0
        );
        assert_eq!(
            value(&samples, "metsuke_submission_bytes_total", &labels),
            bytes as f64
        );
    }
    let landed = value(
        &samples,
        "metsuke_agent_last_accepted_timestamp_seconds",
        &[("pool", &pool), ("version", metsuke_server::CLIENT_VERSION)],
    );
    // The server's clock once the object landed, not the envelope's own.
    assert!(landed >= before, "{landed} is before {before}");
}

/// The reason, and nothing about whose: a refused pool is not one the
/// exposition grows a series for. Undecodable headers are refused before
/// intake sees anything, and counted all the same.
#[test]
fn a_refusal_counts_its_reason_and_names_no_pool() {
    let allowed = test_key();
    let server = Server::over(skew_of(300, &allowed));
    let stranger = other_key();
    assert_eq!(server.submit(&stranger, &envelope_for(&stranger, 1)).0, 403);
    let stale = envelope_at(&allowed, 1, test_now() - time::Duration::days(1));
    assert_eq!(server.submit(&allowed, &stale).0, 400);
    assert_eq!(
        server.post(Attributed::decode(None, None, None), Vec::new()),
        400
    );
    let samples = server.scraped();
    for (reason, count) in [
        ("not_allowlisted", 1.0),
        ("clock_skew", 1.0),
        ("unattributed", 1.0),
        ("bad_signature", 0.0),
    ] {
        let labels = [("reason", reason)];
        assert_eq!(
            value(&samples, "metsuke_submissions_refused_total", &labels),
            count,
            "{reason}"
        );
    }
    let stranger = pool_of(&stranger).to_bech32();
    assert!(
        samples
            .iter()
            .all(|metric| metric.labels.get("pool") != Some(&stranger)),
        "a refused pool reached the exposition"
    );
}

/// The version is the agent's own text, so it is bounded as the journal
/// bounds it and escaped as the format quotes it, and what a scraper decodes
/// is what was bounded.
#[test]
fn a_version_reads_back_as_bounded_whatever_it_holds() {
    let key = test_key();
    let server = Server::over(permissive_config(&[pool_of(&key)]));
    let raw = format!("v1 \"quoted\" back\\slash\nnext {}", "é".repeat(300));
    let mut envelope = envelope_for(&key, 1);
    envelope.agent_version = raw.clone();
    assert_eq!(server.submit(&key, &envelope).0, 200);
    let samples = of(
        &server.scraped(),
        "metsuke_agent_last_accepted_timestamp_seconds",
    );
    let [metric] = samples.as_slice() else {
        panic!("want one version series, got {samples:?}");
    };
    assert_eq!(
        metric.labels.get("version"),
        Some(&metsuke_wire::http::one_line(raw))
    );
}

/// Four versions a pool, the one landed longest ago making room.
#[test]
fn a_pool_shows_its_four_latest_versions() {
    let key = test_key();
    let server = Server::over(permissive_config(&[pool_of(&key)]));
    for counter in 0..6 {
        let mut envelope = envelope_for(&key, counter);
        envelope.agent_version = format!("v{counter}");
        assert_eq!(server.submit(&key, &envelope).0, 200);
    }
    let versions: Vec<String> = of(
        &server.scraped(),
        "metsuke_agent_last_accepted_timestamp_seconds",
    )
    .into_iter()
    .map(|metric| metric.labels["version"].clone())
    .collect();
    assert_eq!(versions, ["v2", "v3", "v4", "v5"]);
}
