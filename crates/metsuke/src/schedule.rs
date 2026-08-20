//! Upload scheduling: one upload outcome in, the delay until the next
//! attempt out. Pure state-plus-arithmetic so the retry policy — jitter on
//! retryable failures, clamped exponential backoff on rejections — is
//! testable without timers.

use std::time::Duration;

use crate::uploader::UploadOutcome;

pub struct ScheduleConfig {
    pub upload_interval: Duration,
    /// Upper bound on the random spread added to a retryable failure, so a
    /// fleet of agents doesn't stampede a recovering server in step.
    pub jitter_max: Duration,
    /// Clamp on the rejection backoff.
    pub backoff_max: Duration,
}

pub struct Schedule {
    consecutive_rejections: u32,
}

impl Schedule {
    pub fn new() -> Self {
        Schedule {
            consecutive_rejections: 0,
        }
    }

    /// The delay until the next upload attempt. `entropy` seeds the jitter;
    /// any caller-supplied noise (e.g. clock nanoseconds) is enough — the
    /// spread only has to differ across agents, not be unpredictable.
    pub fn after(
        &mut self,
        outcome: &UploadOutcome,
        config: &ScheduleConfig,
        entropy: u64,
    ) -> Duration {
        match outcome {
            UploadOutcome::Acked(_) => {
                self.consecutive_rejections = 0;
                config.upload_interval
            }
            UploadOutcome::Retryable(_) => {
                // Deliberate reset: the backoff answers 4xx rejections, and
                // a retryable failure means the latest attempt did not
                // observe one.
                self.consecutive_rejections = 0;
                config.upload_interval + jitter(config.jitter_max, entropy)
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

/// A duration in `[0, max]` derived uniformly-enough from `entropy`.
fn jitter(max: Duration, entropy: u64) -> Duration {
    let span = max.as_millis() as u64 + 1;
    Duration::from_millis(entropy % span)
}
