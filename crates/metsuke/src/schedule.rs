//! Upload scheduling: one upload outcome in, the delay until the next
//! attempt out. Pure state-plus-arithmetic, so the retry policy (jitter on
//! retryable failures, clamped exponential backoff on rejections) is
//! testable without timers.

use std::time::Duration;

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::uploader::UploadOutcome;

/// When the next submission is, for the operator reading the tick that just
/// finished. Neither the configured interval nor guessable from it, because
/// jitter has placed this agent inside it and a refusal replaces it with a
/// backoff.
pub fn next_upload_line(now: OffsetDateTime, wait: Duration) -> String {
    let at = now + wait;
    let at = at
        .replace_nanosecond(0)
        .unwrap_or(at)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "a time this clock cannot render".to_string());
    format!("the next submission is scheduled at {at}")
}

pub struct ScheduleConfig {
    pub upload_interval: Duration,
    /// Upper bound on the spread that places an agent within the interval, and
    /// on the spread a retry adds, so a fleet does not upload in step.
    pub jitter_max: Duration,
    /// Clamp on the rejection backoff.
    pub backoff_max: Duration,
}

pub struct Schedule {
    consecutive_rejections: u32,
    /// Whether this run has already picked where in the interval its uploads
    /// land. Set by the first accepted submission and never read again.
    phase_chosen: bool,
}

impl Schedule {
    pub fn new() -> Self {
        Schedule {
            consecutive_rejections: 0,
            phase_chosen: false,
        }
    }

    /// The delay until the next upload attempt. `entropy` seeds the jitter;
    /// any caller-supplied noise (e.g. clock nanoseconds) is enough. The
    /// spread only has to differ across agents, not be unpredictable.
    pub fn after(
        &mut self,
        outcome: &UploadOutcome,
        config: &ScheduleConfig,
        entropy: u64,
    ) -> Duration {
        match outcome {
            // The first one spreads this agent within the interval, every one
            // after it keeps the phase that chose. Agents installed together
            // would otherwise upload in the same second of every interval for
            // as long as they ran, and nothing in a working fleet would break
            // the step; jittering every time instead would spread them at the
            // cost of the dependable hourly line an operator watches for.
            UploadOutcome::Acked(_) => {
                self.consecutive_rejections = 0;
                match std::mem::replace(&mut self.phase_chosen, true) {
                    false => {
                        config.upload_interval
                            + jitter(config.jitter_max, config.upload_interval, entropy)
                    }
                    true => config.upload_interval,
                }
            }
            UploadOutcome::Retryable(_) => {
                // Deliberate reset: the backoff answers 4xx rejections, and
                // a retryable failure means the latest attempt did not
                // observe one.
                self.consecutive_rejections = 0;
                config.upload_interval + jitter(config.jitter_max, config.upload_interval, entropy)
            }
            UploadOutcome::Rejected { .. } => {
                self.consecutive_rejections = self.consecutive_rejections.saturating_add(1);
                config
                    .upload_interval
                    .saturating_mul(2u32.saturating_pow(self.consecutive_rejections))
                    .min(config.backoff_max)
            }
        }
    }
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule::new()
    }
}

/// A duration in `[0, max]` derived uniformly-enough from `entropy`, never
/// wider than the interval it is spreading agents across. A spread past one
/// interval places nobody better than a spread of exactly one does, and the
/// shipped bound is sized against the shipped interval: on a short one it
/// would be most of the wait rather than a spread inside it.
fn jitter(max: Duration, interval: Duration, entropy: u64) -> Duration {
    let span = max.min(interval).as_millis() as u64 + 1;
    Duration::from_millis(entropy % span)
}
