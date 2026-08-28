//! The `Scrape` every crate's suite spools when it needs a row rather than a
//! scrape of anything real. It lives here because all four suites need it and
//! only this crate is on all four paths, so a field added to `Scrape` is one
//! edit and not four.

use std::collections::BTreeMap;

use time::OffsetDateTime;

use crate::envelope::{Metric, Scrape};

/// The node's block-height gauge, as its PrometheusSimple backend names it.
pub const BLOCK_NUMBER: &str = "cardano_node_metrics_blockNum_int";

/// A scrape carrying `BLOCK_NUMBER` and nothing else. `block_number` is what
/// tells two rows of the same shape apart, so a caller spooling several gives
/// each its own.
pub fn block_number_scrape(scraped_at: OffsetDateTime, block_number: u64) -> Scrape {
    Scrape {
        scraped_at,
        clock_offset_ms: None,
        failure: None,
        metrics: vec![Metric {
            name: BLOCK_NUMBER.to_string(),
            labels: BTreeMap::new(),
            value: block_number.into(),
            declared_type: Some("gauge".to_string()),
        }],
    }
}
