//! Two budgets counted in fixed windows in memory: what one pool may upload,
//! and what every pool together may. The clock is a parameter, so a limit is
//! testable without sleeping and one submission is judged against one clock
//! reading.

use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64};

use metsuke_wire::envelope::PoolId;
use time::OffsetDateTime;

/// What charging an upload cost. The two refusals are different news: one pool
/// is over its own budget, or the server is over the one they share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charged {
    Allowed,
    PoolIsOver,
    ServerIsOver,
}

pub struct RateLimiter {
    max_per_pool: u32,
    max_total: u32,
    window: time::Duration,
    pools: HashMap<PoolId, Window>,
    total: Window,
}

struct Window {
    started_at: OffsetDateTime,
    used: u32,
}

impl Window {
    /// The window as it stands at `now`, restarted if `now` is past it.
    fn at(&mut self, now: OffsetDateTime, window: time::Duration) -> &mut Window {
        if now - self.started_at >= window {
            self.started_at = now;
            self.used = 0;
        }
        self
    }

    fn spend(&mut self, max: u32) -> bool {
        if self.used >= max {
            return false;
        }
        self.used += 1;
        true
    }
}

impl RateLimiter {
    pub fn new(
        max_per_pool: NonZeroU32,
        max_total: NonZeroU32,
        window_secs: NonZeroU64,
    ) -> RateLimiter {
        RateLimiter {
            max_per_pool: max_per_pool.get(),
            max_total: max_total.get(),
            window: time::Duration::seconds(window_secs.get() as i64),
            pools: HashMap::new(),
            // Started at the epoch rather than at construction, so the first
            // upload restarts it: a limiter built an hour before it is used
            // would otherwise begin mid-window.
            total: Window {
                started_at: OffsetDateTime::UNIX_EPOCH,
                used: 0,
            },
        }
    }

    /// Charge one upload to `pool` and to the server.
    ///
    /// The pool's own budget is charged first, so a runaway agent is refused
    /// against its own limit rather than spending the budget every other pool
    /// shares. Nothing is charged once something refuses.
    pub fn charge(&mut self, pool: PoolId, now: OffsetDateTime) -> Charged {
        let window = self.window;
        let pool_window = self
            .pools
            .entry(pool)
            .or_insert(Window {
                started_at: now,
                used: 0,
            })
            .at(now, window);
        if !pool_window.spend(self.max_per_pool) {
            return Charged::PoolIsOver;
        }
        if !self.total.at(now, window).spend(self.max_total) {
            // The pool's charge stands: it asked, and the answer it gets says
            // the window is full. Refunding it would let a pool retry against
            // its own budget for as long as the server stays busy.
            return Charged::ServerIsOver;
        }
        Charged::Allowed
    }
}
