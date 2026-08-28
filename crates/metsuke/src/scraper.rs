//! One whole Scrape: `scrape_once` owns the metrics read, the SNTP probe, and
//! their merge, so "clock offset comes from the agent's own SNTP query"
//! (ADR 0007) is a code path, not caller discipline.

use crate::scrape::{self, ScrapeConfig};
use crate::sntp::{SntpConfig, probe};
use metsuke_wire::envelope::Scrape;

pub struct ScraperConfig {
    pub scrape: ScrapeConfig,
    pub sntp: SntpConfig,
}

/// One complete row: the scrape with `clock_offset_ms` filled from the SNTP
/// probe. Never errors: neither half has a failure a row cannot carry.
pub fn scrape_once(config: &ScraperConfig) -> Scrape {
    let mut row = scrape::scrape(&config.scrape);
    row.clock_offset_ms = probe(&config.sntp);
    row
}
