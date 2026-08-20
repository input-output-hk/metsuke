//! Sample assembly: one `sample()` owns the metrics scrape, the SNTP probe,
//! and their merge, so "clock offset comes from the agent's own SNTP query"
//! (ADR 0007) is a code path, not caller discipline.

use crate::envelope::Sample;
use crate::scrape::{ScrapeConfig, scrape};
use crate::sntp::{SntpConfig, probe};

pub struct SamplerConfig {
    pub scrape: ScrapeConfig,
    pub sntp: SntpConfig,
}

/// One complete sample: the scraped metrics with `clock_offset_ms` filled
/// from the SNTP probe. Never errors: either half degrades to null fields.
pub fn sample(config: &SamplerConfig) -> Sample {
    let mut sample = scrape(&config.scrape);
    sample.clock_offset_ms = probe(&config.sntp);
    sample
}
