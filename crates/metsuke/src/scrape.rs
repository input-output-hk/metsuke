//! One PrometheusSimple scrape turned into one `Sample`. Never errors: an
//! unreachable endpoint, bad status, oversized body, or missing/malformed
//! metric degrades to null fields, because a failed scrape is itself signal
//! (ADR 0007).

use std::time::Duration;

use time::OffsetDateTime;

use crate::envelope::Sample;

/// Metric names as emitted by the Leios node's PrometheusSimple backend,
/// frozen against the recorded fixtures under tests/fixtures/recordings/.
const BLOCK_HEIGHT: &str = "cardano_node_metrics_blockNum_int";
const SLOT: &str = "cardano_node_metrics_slotNum_int";
const SLOT_IN_EPOCH: &str = "cardano_node_metrics_slotInEpoch_int";
const EPOCH: &str = "cardano_node_metrics_epoch_int";
const BUILD_INFO: &str = "cardano_node_metrics_cardano_build_info";

pub struct ScrapeConfig {
    /// PrometheusSimple endpoint, e.g. `http://127.0.0.1:12798/metrics`.
    pub metrics_url: String,
    /// Whole-request deadline: connect, send, and body read together.
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
    let body = fetch(config);
    sample_from_body(sampled_at, body.as_deref())
}

fn fetch(config: &ScrapeConfig) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(config.timeout))
        .build()
        .into();
    agent
        .get(&config.metrics_url)
        .call()
        .ok()?
        .body_mut()
        .with_config()
        .limit(config.max_body_bytes)
        .read_to_string()
        .ok()
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
        let rest = line.strip_prefix(BUILD_INFO)?;
        let rest = rest.strip_prefix('{')?;
        rest.split_once('}').map(|(labels, _)| labels)
    });
    match labels {
        Some(labels) => (
            label_value(labels, "version"),
            label_value(labels, "revision"),
        ),
        None => (None, None),
    }
}

/// The value of `key` in a Prometheus label list (the text between `{` and
/// `}`), or `None` when absent or malformed.
fn label_value(labels: &str, key: &str) -> Option<String> {
    let mut rest = labels;
    loop {
        let (name, after) = rest.split_once('=')?;
        let after = after.strip_prefix('"')?;
        let (value, tail) = take_quoted(after)?;
        if name == key {
            return Some(value);
        }
        rest = tail.strip_prefix(',')?;
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
