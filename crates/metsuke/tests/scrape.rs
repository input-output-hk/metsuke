//! Tier-2 scrape tests: recorded Leios PrometheusSimple bodies replayed via
//! wiremock (ticket metsuke-4zo.2). Recordings and their refresh policy:
//! tests/fixtures/README.md.

use std::time::Duration;

use metsuke::scrape::{FetchError, Refused, ScrapeConfig, fetch, parse, scrape};
use metsuke_wire::envelope::{Metric, Reason, Scrape};
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

async fn scrape_config(config: ScrapeConfig) -> Scrape {
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
async fn scrape_body(body: &str) -> Scrape {
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

/// A failed scrape: the reason it carries, having asserted the two things every
/// one of them says.
fn failure_of(scrape: &Scrape) -> Reason {
    assert_eq!(scrape.metrics, [], "a failed scrape carries no metric");
    assert_eq!(scrape.clock_offset_ms, None, "the scraper fills the offset");
    scrape
        .failure
        .as_ref()
        .expect("a failure names itself")
        .reason
}

// The whole recorded body reaches the row, which is what the endpoint returned
// and not a selection of it. The count is derived; `parse`'s own tests are
// where a metric's fields are asserted.
#[tokio::test]
async fn a_recorded_body_ships_every_metric_it_states() {
    let scrape = scrape_body(RECORDED_CHAIN).await;
    assert_eq!(scrape.metrics.len(), stated_metrics(RECORDED_CHAIN));
    assert_eq!(scrape.failure, None);
    assert_eq!(scrape.clock_offset_ms, None);
    assert_eq!(
        named(&scrape.metrics, "cardano_node_metrics_blockNum_int")
            .value
            .as_u64(),
        Some(5)
    );
}

/// The node's first served body, before any chain metric is emitted: fewer
/// metrics than the chain recording has, and still a whole scrape.
#[tokio::test]
async fn a_recorded_startup_body_ships_the_metrics_it_has() {
    let scrape = scrape_body(RECORDED_STARTUP).await;
    assert_eq!(scrape.metrics.len(), stated_metrics(RECORDED_STARTUP));
    assert!(scrape.metrics.len() < stated_metrics(RECORDED_CHAIN));
    assert_eq!(scrape.failure, None);
}

// Loopback because `MetricsUrl` refuses anything else, and the discard port
// because it is privileged, so no MockServer can take it (metsuke-4zo.18).
#[tokio::test]
async fn an_endpoint_that_does_not_answer_ships_as_unreachable() {
    let before = time::OffsetDateTime::now_utc();
    let scrape = scrape_config(config("http://127.0.0.1:9/metrics".into())).await;
    assert_eq!(failure_of(&scrape), Reason::Unreachable);
    // The agent's own clock, not the node's: a failed scrape is the one row
    // whose time nothing else could have stamped.
    assert!(
        (before..=time::OffsetDateTime::now_utc()).contains(&scrape.scraped_at),
        "{} is outside the call",
        scrape.scraped_at
    );
}

// A refused connect returns at once, so it cannot show that the deadline is
// the thing bounding a scrape: an endpoint that answers too late must.
#[tokio::test]
async fn an_endpoint_slower_than_the_timeout_ships_as_a_failure() {
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
    let scrape = scrape_config(slow).await;
    assert_eq!(failure_of(&scrape), Reason::Unreachable);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= timeout && elapsed < timeout * 10,
        "the scrape took {elapsed:?}, not the configured {timeout:?}"
    );
}

#[tokio::test]
async fn an_http_error_ships_as_refused() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let scrape = scrape_config(config(format!("{}/metrics", server.uri()))).await;
    assert_eq!(failure_of(&scrape), Reason::Refused);
}

// A refused scrape stays a failed scrape even when the error page carries
// something a metric parser would read.
#[tokio::test]
async fn an_http_error_carrying_metric_lines_ships_no_metric() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(RECORDED_CHAIN, "text/plain"))
        .mount(&server)
        .await;
    let scrape = scrape_config(config(format!("{}/metrics", server.uri()))).await;
    assert_eq!(failure_of(&scrape), Reason::Refused);
}

#[tokio::test]
async fn an_oversized_body_ships_as_too_large() {
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
    let scrape = scrape_config(small).await;
    assert_eq!(failure_of(&scrape), Reason::TooLarge);
}

// The endpoint answering with nothing is not a failure: the row says the
// endpoint was read and stated no metric, which is what a consumer separates
// from a refusal and from no row at all.
#[tokio::test]
async fn an_empty_body_ships_as_a_scrape_with_no_metrics_and_no_failure() {
    let scrape = scrape_body("").await;
    assert_eq!(scrape.metrics, []);
    assert_eq!(scrape.failure, None);
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

// The escapes and metacharacters a label value may carry (`labels_of`), on the
// metric that carries twelve labels on a real node.
#[test]
fn escaped_label_values_decode() {
    let scrape = parse(include_str!("fixtures/edge-cases/escaped-labels.prom"));
    let build_info = named(&scrape.metrics, "cardano_node_metrics_cardano_build_info");
    assert_eq!(
        build_info.labels.get("version").map(String::as_str),
        Some("1\\2\n3")
    );
    assert_eq!(
        build_info.labels.get("extra").map(String::as_str),
        Some("a\"b,c=d")
    );
    assert_eq!(
        build_info.labels.get("revision").map(String::as_str),
        Some("r")
    );
}
