//! Tier-2 scrape tests: recorded Leios PrometheusSimple bodies replayed via
//! wiremock (ticket metsuke-4zo.2). Recordings and their refresh policy:
//! tests/fixtures/README.md.

use std::time::Duration;

use metsuke::scrape::{FetchError, Metric, Refused, ScrapeConfig, fetch, parse, scrape};
use metsuke_wire::envelope::Sample;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");
const RECORDED_STARTUP: &str = include_str!("fixtures/recordings/leios-node-startup.prom");
const RECORDED_TESTNET_BP: &str = include_str!("fixtures/recordings/leios-testnet-bp.prom");

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

async fn fetch_config(config: ScrapeConfig) -> Result<String, FetchError> {
    tokio::task::spawn_blocking(move || fetch(&config))
        .await
        .expect("fetch task panicked")
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

/// Every line of an exposition body that is not a comment or blank states one
/// metric, so this is the count `parse` has to reach on a recorded body.
fn stated_metrics(body: &str) -> usize {
    body.lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .count()
}

fn named<'a>(metrics: &'a [Metric], name: &str) -> &'a Metric {
    metrics
        .iter()
        .find(|metric| metric.name == name)
        .unwrap_or_else(|| panic!("the recording states {name}"))
}

#[test]
fn every_metric_the_recorded_body_states_is_parsed() {
    let scrape = parse(RECORDED_TESTNET_BP);
    assert_eq!(scrape.metrics.len(), stated_metrics(RECORDED_TESTNET_BP));
    assert_eq!(scrape.refused, []);
}

// The two labelled metrics of the recording: an unlabelled parse would reduce
// both to the value 1 and lose everything they carry.
#[test]
fn labelled_metrics_keep_every_label() {
    let scrape = parse(RECORDED_TESTNET_BP);
    let build_info = named(&scrape.metrics, "cardano_node_metrics_cardano_build_info");
    assert_eq!(build_info.labels.len(), 12);
    assert_eq!(
        build_info.labels.get("revision").map(String::as_str),
        Some("3e1bec0217b1560827956d5973120bbff983ee96")
    );
    let tip = named(&scrape.metrics, "cardano_node_metrics_tipBlock");
    assert_eq!(tip.labels.len(), 3);
    assert!(tip.labels.contains_key("issuer_verification_key_hash"));
}

#[test]
fn a_metric_the_body_typed_carries_its_type_and_an_untyped_one_none() {
    let scrape = parse(RECORDED_TESTNET_BP);
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_blockNum_int")
            .declared_type
            .as_deref(),
        Some("gauge")
    );
    assert_eq!(
        named(
            &scrape.metrics,
            "cardano_node_metrics_blockReplayProgress_real"
        )
        .declared_type,
        None
    );
}

#[test]
fn integer_metrics_stay_integers_and_real_ones_floats() {
    let scrape = parse(RECORDED_TESTNET_BP);
    let allocated = &named(&scrape.metrics, "rts_gc_bytes_allocated").value;
    assert_eq!(allocated.as_u64(), Some(3_893_758_466_080));
    assert!(allocated.is_u64(), "{allocated} is not an integer");
    let replay = &named(
        &scrape.metrics,
        "cardano_node_metrics_blockReplayProgress_real",
    )
    .value;
    assert_eq!(replay.as_f64(), Some(99.99831874795292));
}

#[test]
fn non_finite_values_are_dropped_named_and_the_rest_kept() {
    let scrape = parse(include_str!("fixtures/edge-cases/non-finite-values.prom"));
    assert_eq!(
        scrape.refused,
        [
            Refused::NonFinite {
                name: "cardano_node_metrics_density_real".to_string(),
                value: "NaN".to_string(),
            },
            Refused::NonFinite {
                name: "cardano_node_metrics_blockReplayProgress_real".to_string(),
                value: "+Inf".to_string(),
            },
        ]
    );
    assert_eq!(scrape.metrics.len(), 1);
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_blockNum_int")
            .value
            .as_u64(),
        Some(42)
    );
}

// Refusal is per line, so the readable blockNum line survives the unreadable
// one beside it.
#[test]
fn a_malformed_body_yields_the_lines_it_can_and_names_the_rest() {
    let body = include_str!("fixtures/edge-cases/malformed-values.prom");
    let scrape = parse(body);
    assert_eq!(
        scrape.refused,
        [Refused::Unreadable {
            line: "cardano_node_metrics_blockNum_int not-a-number".to_string(),
        }]
    );
    assert_eq!(
        scrape.metrics.len(),
        stated_metrics(body) - scrape.refused.len()
    );
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_blockNum_int")
            .value
            .as_u64(),
        Some(9)
    );
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_slotNum_int")
            .value
            .as_f64(),
        Some(1.5)
    );
}

#[test]
fn an_empty_body_states_no_metrics_and_refuses_nothing() {
    let scrape = parse("");
    assert!(scrape.metrics.is_empty());
    assert_eq!(scrape.refused, []);
}

// The exposition format allows a scrape time after the value; the agent times
// its own scrape, so such a line is a metric like any other.
#[test]
fn a_line_carrying_its_own_timestamp_still_yields_its_metric() {
    let scrape = parse("cardano_node_metrics_blockNum_int 42 1596151461000\n");
    assert_eq!(scrape.refused, []);
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_blockNum_int")
            .value
            .as_u64(),
        Some(42)
    );
}

#[tokio::test]
async fn a_body_past_the_limit_is_named_as_such() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(RECORDED_CHAIN, "text/plain"))
        .mount(&server)
        .await;
    let mut small = config(format!("{}/metrics", server.uri()));
    small.max_body_bytes = 16;
    let error = fetch_config(small)
        .await
        .expect_err("16 bytes is not a body");
    assert!(
        matches!(error, FetchError::TooLarge { limit: 16 }),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_error_page_is_named_with_its_status_and_reason() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(503).set_body_raw("upstream is down", "text/plain"))
        .mount(&server)
        .await;
    let error = fetch_config(config(format!("{}/metrics", server.uri())))
        .await
        .expect_err("503 is not a metrics body");
    match error {
        FetchError::Refused(refusal) => {
            assert_eq!(refusal.status, 503);
            assert_eq!(refusal.reason, "upstream is down");
            assert!(refusal.retryable);
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn an_endpoint_that_does_not_answer_is_named_apart_from_a_refusal() {
    let error = fetch_config(config("http://127.0.0.1:9/metrics".into()))
        .await
        .expect_err("the discard port answers nothing");
    assert!(matches!(error, FetchError::Unreachable(_)), "{error:?}");
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
