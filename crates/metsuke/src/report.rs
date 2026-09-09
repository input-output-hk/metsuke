//! How often the scrape tick says a thing. The first of each kind is named as
//! it happens, so a broken endpoint is not held back for an upload interval;
//! the rest are counted and said once on the boundary the upload tick sets,
//! because a node that stays down would otherwise write the same line every
//! scrape interval for as long as it is down.
//!
//! Lines come back rather than reaching the journal from here, so the policy is
//! testable without a subprocess and the severity prefixes stay in one place.

use crate::agent::ScrapeNews;

/// One line to log, and how loudly. The prefixes stay in `main`; which of them
/// a line takes is this module's to decide, because that is part of the policy
/// it holds.
#[derive(Debug, PartialEq)]
pub enum Line {
    Info(String),
    Warning(String),
}

/// What the scrape tick has met since the last `drain`.
#[derive(Debug, Default, PartialEq)]
pub struct ScrapeReport {
    since: Since,
    /// Scrapes that have failed since the last one that answered, which
    /// `drain` does not clear: an endpoint can go down and come back across
    /// an upload boundary, and the line that says it is back has to be able
    /// to say how much it missed.
    failing: u64,
    /// Whether any scrape has answered. An endpoint that has never answered
    /// is a node still starting; one that has is a node that stopped.
    answered: bool,
}

/// The counters an upload tick zeroes, which is every one a summary is of.
#[derive(Debug, Default, PartialEq)]
struct Since {
    /// Scrapes that failed, each shipped as a failure.
    failed: u64,
    /// Scrapes whose body held lines no metric was read from, and how many
    /// lines that was across them.
    refusing: u64,
    refused_lines: u64,
}

impl ScrapeReport {
    /// Whether the endpoint has answered a scrape yet. What the loop waits on
    /// before it settles into the configured interval.
    pub fn answered(&self) -> bool {
        self.answered
    }

    /// Take one tick's news, and what to say about it now. `starting` is the
    /// window in which the node is expected to be coming up alongside the
    /// agent, which is every pipe-mode start: a failure there is news at
    /// Info, and only a failure past it is a warning.
    pub fn record(&mut self, news: &ScrapeNews, starting: bool) -> Vec<Line> {
        let mut lines = Vec::new();
        match &news.failed {
            Some(detail) => {
                self.since.failed += 1;
                self.failing += 1;
                if self.since.failed == 1 {
                    lines.push(match starting && !self.answered {
                        true => Line::Info(format!(
                            "the node's metrics endpoint has not answered yet, so this scrape \
                             ships as a failure: {detail}"
                        )),
                        false => Line::Warning(format!(
                            "the scrape failed, and the failure is what ships: {detail}"
                        )),
                    });
                }
            }
            None => {
                self.answered = true;
                // The one success worth a line: an operator watching a fixed
                // endpoint would otherwise learn it recovered from an upload
                // an interval later.
                if self.failing > 0 {
                    lines.push(Line::Info(format!(
                        "the node's metrics endpoint answered, after {} that did not",
                        failed_scrapes(self.failing)
                    )));
                    self.failing = 0;
                }
            }
        }
        if let Some(first) = news.refused.first() {
            self.since.refusing += 1;
            self.since.refused_lines += news.refused.len() as u64;
            if self.since.refusing == 1 {
                lines.push(Line::Warning(format!(
                    "{} of the node's metric lines did not ship, the first: {first}",
                    news.refused.len()
                )));
            }
        }
        lines
    }

    /// What the scrapes since the last call added up to, and zero from here.
    /// Silent about a kind that happened once, because the line naming it as it
    /// happened already said so.
    pub fn drain(&mut self) -> Vec<Line> {
        let since = std::mem::take(&mut self.since);
        let mut lines = Vec::new();
        if since.failed > 1 {
            lines.push(Line::Warning(format!(
                "{} scrapes have failed since the last report, each shipping as a failure",
                since.failed
            )));
        }
        if since.refusing > 1 {
            lines.push(Line::Warning(format!(
                "{} of the node's metric lines did not ship, across {} scrapes since the \
                 last report",
                since.refused_lines, since.refusing
            )));
        }
        lines
    }
}

/// The count as the recovery line reads it, which is a sentence rather than a
/// table.
fn failed_scrapes(count: u64) -> String {
    match count {
        1 => "one scrape".to_string(),
        many => format!("{many} scrapes"),
    }
}
