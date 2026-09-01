//! How often the scrape tick says a thing. The first of each kind is named as
//! it happens, so a broken endpoint is not held back for an upload interval;
//! the rest are counted and said once on the boundary the upload tick sets,
//! because a node that stays down would otherwise write the same line every
//! scrape interval for as long as it is down.
//!
//! Lines come back rather than reaching the journal from here, so the policy is
//! testable without a subprocess and the severity prefixes stay in one place.

use crate::agent::ScrapeNews;

/// What the scrape tick has met since the last `drain`.
#[derive(Debug, Default, PartialEq)]
pub struct ScrapeReport {
    /// Scrapes that failed, each shipped as a failure.
    failed: u64,
    /// Scrapes whose body held lines no metric was read from, and how many
    /// lines that was across them.
    refusing: u64,
    refused_lines: u64,
}

impl ScrapeReport {
    /// Take one tick's news, and what to warn about it now.
    pub fn record(&mut self, news: &ScrapeNews) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(detail) = &news.failed {
            self.failed += 1;
            if self.failed == 1 {
                lines.push(format!("the scrape failed and ships as one: {detail}"));
            }
        }
        if let Some(first) = news.refused.first() {
            self.refusing += 1;
            self.refused_lines += news.refused.len() as u64;
            if self.refusing == 1 {
                lines.push(format!(
                    "{} of the node's metric lines did not ship, the first: {first}",
                    news.refused.len()
                ));
            }
        }
        lines
    }

    /// What the scrapes since the last call added up to, and zero from here.
    /// Silent about a kind that happened once, because the line naming it as it
    /// happened already said so.
    pub fn drain(&mut self) -> Vec<String> {
        let report = std::mem::take(self);
        let mut lines = Vec::new();
        if report.failed > 1 {
            lines.push(format!(
                "{} scrapes have failed since the last report, each shipping as a failure",
                report.failed
            ));
        }
        if report.refusing > 1 {
            lines.push(format!(
                "{} of the node's metric lines did not ship, across {} scrapes since the \
                 last report",
                report.refused_lines, report.refusing
            ));
        }
        lines
    }
}
