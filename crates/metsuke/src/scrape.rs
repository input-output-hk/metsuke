//! Reading one PrometheusSimple endpoint: the socket half and the text half,
//! together because neither is used without the other. `scrape` is the v1 path
//! across both and never errors, because a failed scrape is itself signal
//! (ADR 0007).

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Number;
use time::OffsetDateTime;

use crate::endpoint::MetricsUrl;
use metsuke_wire::envelope::Sample;
use metsuke_wire::http;

/// Metric names as emitted by the Leios node's PrometheusSimple backend,
/// frozen against the recorded fixtures under tests/fixtures/recordings/.
const BLOCK_HEIGHT: &str = "cardano_node_metrics_blockNum_int";
const SLOT: &str = "cardano_node_metrics_slotNum_int";
const SLOT_IN_EPOCH: &str = "cardano_node_metrics_slotInEpoch_int";
const EPOCH: &str = "cardano_node_metrics_epoch_int";
const BUILD_INFO: &str = "cardano_node_metrics_cardano_build_info";

pub struct ScrapeConfig {
    pub metrics_url: MetricsUrl,
    /// Whole-request deadline, as bounded by `metsuke_wire::http::agent`.
    pub timeout: Duration,
    /// A body larger than this is treated as a failed scrape.
    pub max_body_bytes: u64,
}

/// Scrape once. `clock_offset_ms` stays null here — the sampler fills it
/// from the SNTP probe. `sync_progress` also stays null: the
/// Leios PrometheusSimple endpoint exposes no sync-progress metric (see the
/// recorded fixtures), so v1 has nothing to fill it from.
pub fn scrape(config: &ScrapeConfig) -> Sample {
    let sampled_at = OffsetDateTime::now_utc();
    let body = fetch(config).ok();
    sample_from_body(sampled_at, body.as_deref())
}

/// Why a body did not arrive. Kept apart because they call for different
/// answers: one may come back, one is the endpoint's own reason, one is a
/// limit this agent set, and one is an answer that broke off mid-body.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("the endpoint did not answer: {0}")]
    Unreachable(#[source] ureq::Error),
    #[error("the endpoint answered {} ({})", .0.status, .0.reason)]
    Refused(http::Refusal),
    #[error("the body is past the configured limit of {limit} bytes")]
    TooLarge { limit: u64 },
    #[error("the answer's body did not read: {0}")]
    Unreadable(#[source] ureq::Error),
}

/// One body off the endpoint, or why there is none.
pub fn fetch(config: &ScrapeConfig) -> Result<String, FetchError> {
    let mut response = http::agent(config.timeout)
        .get(config.metrics_url.as_str())
        .call()
        .map_err(FetchError::Unreachable)?;
    // An error page is not a metrics body, whatever its lines parse as.
    http::classify(&mut response).map_err(FetchError::Refused)?;
    response
        .body_mut()
        .with_config()
        .limit(config.max_body_bytes)
        .read_to_string()
        .map_err(|error| match error {
            ureq::Error::BodyExceedsLimit(limit) => FetchError::TooLarge { limit },
            other => FetchError::Unreadable(other),
        })
}

/// What one exposition body yielded: the metrics it stated, and every line
/// that yielded none.
#[derive(Debug, PartialEq)]
pub struct ParsedBody {
    pub metrics: Vec<Metric>,
    pub refused: Vec<Refused>,
}

/// A line that reached no metric, carrying what a caller needs to name it.
/// The two are apart because they say different things about the node: one
/// stated a value nothing can hold, the other wrote something no metric is.
#[derive(Debug, PartialEq)]
pub enum Refused {
    NonFinite { name: String, value: String },
    Unreadable { line: String },
}

/// One exposition line: what the endpoint called it, what it carried, and the
/// type the body declared for it — some metrics arrive with no `# TYPE` line.
#[derive(Debug, PartialEq)]
pub struct Metric {
    pub name: String,
    pub labels: BTreeMap<String, String>,
    pub value: Number,
    pub declared_type: Option<String>,
}

/// Every metric an exposition body states, one per line — this endpoint groups
/// nothing under a shared name. Values keep the shape the body wrote them in:
/// an integer stays an integer, so a counter past an f64's exact range keeps
/// its digits.
pub fn parse(body: &str) -> ParsedBody {
    let types = declared_types(body);
    let mut metrics = Vec::new();
    let mut refused = Vec::new();
    for line in body.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        match metric(line, &types) {
            Ok(metric) => metrics.push(metric),
            Err(reason) => refused.push(reason),
        }
    }
    ParsedBody { metrics, refused }
}

/// `# TYPE <name> <type>` lines, by metric name.
fn declared_types(body: &str) -> BTreeMap<&str, &str> {
    body.lines()
        .filter_map(|line| {
            let mut words = line.strip_prefix("# TYPE ")?.split_whitespace();
            Some((words.next()?, words.next()?))
        })
        .collect()
}

fn metric(line: &str, types: &BTreeMap<&str, &str>) -> Result<Metric, Refused> {
    let unreadable = || Refused::Unreadable {
        line: line.to_string(),
    };
    let name_end = line
        .find(['{', ' '])
        .filter(|end| *end > 0)
        .ok_or_else(unreadable)?;
    let (name, rest) = line.split_at(name_end);
    let (labels, rest) = match rest.strip_prefix('{') {
        Some(rest) => labels_of(rest).ok_or_else(unreadable)?,
        None => (BTreeMap::new(), rest),
    };
    // A line may state the scrape time after its value; the agent times its
    // own scrape, so that field is read past rather than shipped.
    let value = rest.split_whitespace().next().ok_or_else(unreadable)?;
    Ok(Metric {
        name: name.to_string(),
        labels,
        value: number(value).ok_or_else(|| match value.parse::<f64>() {
            Ok(_) => Refused::NonFinite {
                name: name.to_string(),
                value: value.to_string(),
            },
            Err(_) => unreadable(),
        })?,
        declared_type: types.get(name).map(|declared| declared.to_string()),
    })
}

/// A JSON number, or `None` for a value JSON cannot hold: a non-finite float
/// serializes as `null`, which reads as a metric the endpoint never stated.
fn number(text: &str) -> Option<Number> {
    if let Ok(value) = text.parse::<u64>() {
        return Some(value.into());
    }
    if let Ok(value) = text.parse::<i64>() {
        return Some(value.into());
    }
    Number::from_f64(text.parse::<f64>().ok()?)
}

/// A Prometheus label list and what follows it, read from just past the `{`.
/// `None` when the list never closes or a label is malformed. Quoting is the
/// only thing that ends a value, so a `}` or `,` inside one is a label
/// character like any other.
fn labels_of(s: &str) -> Option<(BTreeMap<String, String>, &str)> {
    let mut labels = BTreeMap::new();
    let mut rest = s;
    loop {
        if let Some(after) = rest.strip_prefix('}') {
            return Some((labels, after));
        }
        let (name, after) = rest.split_once('=')?;
        let (value, after) = take_quoted(after.strip_prefix('"')?)?;
        labels.insert(name.to_string(), value);
        rest = after.strip_prefix(',').unwrap_or(after);
    }
}

/// A `None` body means the fetch itself failed.
fn sample_from_body(sampled_at: OffsetDateTime, body: Option<&str>) -> Sample {
    let (node_version, node_revision) = body.map(build_info).unwrap_or_default();
    Sample {
        sampled_at,
        block_height: body.and_then(|b| int_metric(b, BLOCK_HEIGHT)),
        slot: body.and_then(|b| int_metric(b, SLOT)),
        slot_in_epoch: body.and_then(|b| int_metric(b, SLOT_IN_EPOCH)),
        epoch: body.and_then(|b| int_metric(b, EPOCH)),
        sync_progress: None,
        node_version,
        node_revision,
        clock_offset_ms: None,
    }
}

/// The value of a label-less integer gauge, by exact metric name.
fn int_metric(body: &str, name: &str) -> Option<u64> {
    body.lines().find_map(|line| {
        let rest = line.strip_prefix(name)?;
        // The space rejects longer names sharing `name` as a prefix, and
        // labelled variants (which would continue with `{`).
        let value = rest.strip_prefix(' ')?;
        value.trim().parse().ok()
    })
}

/// `version` and `revision` labels of the build-info metric.
fn build_info(body: &str) -> (Option<String>, Option<String>) {
    let labels = body.lines().find_map(|line| {
        let rest = line.strip_prefix(BUILD_INFO)?.strip_prefix('{')?;
        labels_of(rest).map(|(labels, _)| labels)
    });
    match labels {
        Some(mut labels) => (labels.remove("version"), labels.remove("revision")),
        None => (None, None),
    }
}

/// Consume a label value up to its closing quote, decoding the Prometheus
/// text-format escapes `\\`, `\"`, and `\n`.
fn take_quoted(s: &str) -> Option<(String, &str)> {
    let mut value = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((value, &s[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => value.push('\n'),
                escaped => value.push(escaped),
            },
            plain => value.push(plain),
        }
    }
    None
}
