//! Tier-2 scrape tests: recorded Leios PrometheusSimple bodies replayed via
//! wiremock (ticket metsuke-4zo.2). Recordings and their refresh policy:
//! tests/fixtures/README.md.

use std::time::Duration;

use metsuke::scrape::{ScrapeConfig, scrape};
use metsuke_wire::envelope::Sample;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");
const RECORDED_STARTUP: &str = include_str!("fixtures/recordings/leios-node-startup.prom");

fn config(metrics_url: String) -> ScrapeConfig {
    ScrapeConfig {
        metrics_url: metrics_url.try_into().unwrap(),
        timeout: Duration::from_secs(5),
        max_body_bytes: 1024 * 1024,
    }
}

async fn scrape_config(config: ScrapeConfig) -> Sample {
    tokio::task::spawn_blocking(move || scrape(&config))
        .await
        .expect("scrape task panicked")
}

/// Serve `body` on a wiremock endpoint and scrape it.
async fn scrape_body(body: &str) -> Sample {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(body, "text/plain;version=0.0.4;charset=utf-8"),
        )
        .mount(&server)
        .await;
    scrape_config(config(format!("{}/metrics", server.uri()))).await
}

// Expected values of the committed recording; the re-record script prints
// the replacements.
#[tokio::test]
async fn recorded_chain_body_yields_field_values() {
    let sample = scrape_body(RECORDED_CHAIN).await;
    assert_eq!(sample.block_height, Some(5));
    assert_eq!(sample.slot, Some(250));
    assert_eq!(sample.slot_in_epoch, Some(250));
    assert_eq!(sample.epoch, Some(0));
    assert_eq!(sample.node_version.as_deref(), Some("11.1.0.164"));
    assert_eq!(
        sample.node_revision.as_deref(),
        Some("c5f7d9121beb67e6d03d0632ae2bd3b76259728e")
    );
    // Why these two stay null: the scrape() doc.
    assert_eq!(sample.sync_progress, None);
    assert_eq!(sample.clock_offset_ms, None);
}

/// The node's first served body, before any chain metric is emitted: build
/// info is already present, chain fields are null.
#[tokio::test]
async fn recorded_startup_body_yields_build_info_only() {
    let sample = scrape_body(RECORDED_STARTUP).await;
    assert_eq!(sample.block_height, None);
    assert_eq!(sample.slot, None);
    assert_eq!(sample.slot_in_epoch, None);
    assert_eq!(sample.epoch, None);
    assert_eq!(sample.node_version.as_deref(), Some("11.1.0.164"));
    assert_eq!(
        sample.node_revision.as_deref(),
        Some("c5f7d9121beb67e6d03d0632ae2bd3b76259728e")
    );
}

// Loopback because `MetricsUrl` refuses anything else, and the discard port
// because it is privileged, so no MockServer can take it (metsuke-4zo.18).
#[tokio::test]
async fn refused_endpoint_yields_all_nulls() {
    let sample = scrape_config(config("http://127.0.0.1:9/metrics".into())).await;
    assert_eq!(sample, all_null(sample.sampled_at));
}

// A refused connect returns at once, so it cannot show that the deadline is
// the thing bounding a scrape: an endpoint that answers too late must.
#[tokio::test]
async fn endpoint_slower_than_the_timeout_yields_all_nulls() {
    let server = MockServer::start().await;
    let timeout = Duration::from_millis(200);
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(RECORDED_CHAIN, "text/plain")
                .set_delay(timeout * 10),
        )
        .mount(&server)
        .await;
    let mut slow = config(format!("{}/metrics", server.uri()));
    slow.timeout = timeout;
    let started = std::time::Instant::now();
    let sample = scrape_config(slow).await;
    assert_eq!(sample, all_null(sample.sampled_at));
    let elapsed = started.elapsed();
    assert!(
        elapsed >= timeout && elapsed < timeout * 10,
        "the scrape took {elapsed:?}, not the configured {timeout:?}"
    );
}

#[tokio::test]
async fn http_error_yields_all_nulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let sample = scrape_config(config(format!("{}/metrics", server.uri()))).await;
    assert_eq!(sample, all_null(sample.sampled_at));
}

// A refused scrape stays a failed scrape even when the error page carries
// something a metric parser would read.
#[tokio::test]
async fn http_error_carrying_metric_lines_yields_all_nulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(RECORDED_CHAIN, "text/plain"))
        .mount(&server)
        .await;
    let sample = scrape_config(config(format!("{}/metrics", server.uri()))).await;
    assert_eq!(sample, all_null(sample.sampled_at));
}

#[tokio::test]
async fn oversized_body_yields_all_nulls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(RECORDED_CHAIN, "text/plain;version=0.0.4;charset=utf-8"),
        )
        .mount(&server)
        .await;
    let mut small = config(format!("{}/metrics", server.uri()));
    small.max_body_bytes = 16;
    let sample = scrape_config(small).await;
    assert_eq!(sample, all_null(sample.sampled_at));
}

#[tokio::test]
async fn empty_body_yields_all_nulls() {
    let sample = scrape_body("").await;
    assert_eq!(sample, all_null(sample.sampled_at));
}

#[tokio::test]
async fn build_info_without_revision_yields_version_only() {
    let sample = scrape_body(include_str!(
        "fixtures/edge-cases/build-info-missing-revision.prom"
    ))
    .await;
    assert_eq!(sample.node_version.as_deref(), Some("99.0.0"));
    assert_eq!(sample.node_revision, None);
    assert_eq!(sample.block_height, Some(42));
}

#[tokio::test]
async fn malformed_values_yield_nulls_per_field() {
    let sample = scrape_body(include_str!("fixtures/edge-cases/malformed-values.prom")).await;
    assert_eq!(sample.block_height, None);
    assert_eq!(sample.slot, None);
    assert_eq!(sample.epoch, None);
    assert_eq!(sample.slot_in_epoch, Some(7));
    assert_eq!(sample.node_version.as_deref(), Some("9"));
    assert_eq!(sample.node_revision.as_deref(), Some("abc"));
}

#[tokio::test]
async fn escaped_label_values_decode() {
    let sample = scrape_body(include_str!("fixtures/edge-cases/escaped-labels.prom")).await;
    assert_eq!(sample.node_version.as_deref(), Some("1\\2\n3"));
    assert_eq!(sample.node_revision.as_deref(), Some("r"));
}

fn all_null(sampled_at: time::OffsetDateTime) -> Sample {
    Sample {
        sampled_at,
        block_height: None,
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}
