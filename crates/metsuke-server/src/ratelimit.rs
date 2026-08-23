//! A per-pool budget, counted in a fixed window in memory: uploads on the
//! ingest path, directory lookups behind it. The clock is a parameter, so a
//! limit is testable without sleeping and one submission is judged against one
//! clock reading.

use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64};

use metsuke_wire::envelope::PoolId;
use time::OffsetDateTime;

pub struct RateLimiter {
    max_per_window: u32,
    window: time::Duration,
    windows: HashMap<PoolId, Window>,
}

struct Window {
    started_at: OffsetDateTime,
    used: u32,
}

impl RateLimiter {
    pub fn new(max_per_window: NonZeroU32, window_secs: NonZeroU64) -> Self {
        RateLimiter {
            max_per_window: max_per_window.get(),
            window: time::Duration::seconds(window_secs.get() as i64),
            windows: HashMap::new(),
        }
    }

    /// Charge one use to `pool`, and answer whether the window's budget
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
        if window.used >= self.max_per_window {
            return false;
        }
        window.used += 1;
        true
    }
}
