//! Per-pool upload budget: a fixed window counted in memory. The clock is a
//! parameter, so the limit is testable without sleeping and the ingest path
//! keeps one clock reading per submission.

use std::collections::HashMap;

use metsuke::envelope::PoolId;
use time::OffsetDateTime;

pub struct RateLimiter {
    max_uploads: u32,
    window: time::Duration,
    windows: HashMap<PoolId, Window>,
}

struct Window {
    started_at: OffsetDateTime,
    used: u32,
}

impl RateLimiter {
    pub fn new(max_uploads: u32, window_secs: u64) -> Self {
        RateLimiter {
            max_uploads,
            window: time::Duration::seconds(window_secs as i64),
            windows: HashMap::new(),
        }
    }

    /// Charge one upload to `pool`, and answer whether the window's budget
    /// covered it.
    pub fn allow(&mut self, pool: PoolId, now: OffsetDateTime) -> bool {
        let window = self.windows.entry(pool).or_insert(Window {
            started_at: now,
            used: 0,
        });
        if now - window.started_at >= self.window {
            window.started_at = now;
            window.used = 0;
        }
        if window.used >= self.max_uploads {
            return false;
        }
        window.used += 1;
        true
    }
}
