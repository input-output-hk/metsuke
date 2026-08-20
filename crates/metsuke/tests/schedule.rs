//! Upload scheduling tests (ticket metsuke-4zo.5): outcome in, delay until
//! the next attempt out. 5xx/transport retries next interval with jitter;
//! 4xx backs off exponentially with a clamp; an ack resets the backoff.

use std::time::Duration;

use metsuke::envelope::Ack;
use metsuke::schedule::{Schedule, ScheduleConfig};
use metsuke::uploader::UploadOutcome;

fn config() -> ScheduleConfig {
    ScheduleConfig {
        upload_interval: Duration::from_secs(3600),
        jitter_max: Duration::from_secs(60),
        backoff_max: Duration::from_secs(86400),
    }
}

fn acked() -> UploadOutcome {
    UploadOutcome::Acked(Ack {
        latest_version: "0.1.0".into(),
    })
}

fn rejected() -> UploadOutcome {
    UploadOutcome::Rejected {
        status: 403,
        reason: "pool not on the allowlist".into(),
    }
}

#[test]
fn ack_schedules_the_plain_interval() {
    let mut schedule = Schedule::new();
    let delay = schedule.after(&acked(), &config(), 7);
    assert_eq!(delay, Duration::from_secs(3600));
}

// Acceptance: 5xx → retry scheduled with jitter, bounded by jitter_max.
#[test]
fn retryable_schedules_the_interval_plus_bounded_jitter() {
    let interval = Duration::from_secs(3600);
    let jitter_max = Duration::from_secs(60);
    for entropy in [0, 1, 59, 60, 61, u64::MAX] {
        let mut schedule = Schedule::new();
        let delay = schedule.after(&UploadOutcome::Retryable("503".into()), &config(), entropy);
        assert!(
            (interval..=interval + jitter_max).contains(&delay),
            "entropy {entropy}: delay {delay:?} outside [interval, interval + jitter_max]"
        );
    }
}

// Distinct entropy must be able to produce distinct delays, or the jitter
// is decorative.
#[test]
fn jitter_varies_with_entropy() {
    let delays: Vec<Duration> = (0..10)
        .map(|entropy| {
            Schedule::new().after(&UploadOutcome::Retryable("503".into()), &config(), entropy)
        })
        .collect();
    assert!(
        delays.windows(2).any(|pair| pair[0] != pair[1]),
        "ten entropies, one delay: {delays:?}"
    );
}

// Acceptance: 4xx → exponential backoff with a clamp, doubling per
// consecutive rejection.
#[test]
fn consecutive_rejections_double_the_delay_up_to_the_clamp() {
    let mut schedule = Schedule::new();
    let hours: Vec<u64> = (0..5)
        .map(|_| schedule.after(&rejected(), &config(), 0).as_secs() / 3600)
        .collect();
    assert_eq!(hours, vec![2, 4, 8, 16, 24]);
}

// Backoff far past the clamp must not overflow the doubling arithmetic.
#[test]
fn backoff_stays_clamped_after_many_rejections() {
    let mut schedule = Schedule::new();
    for _ in 0..200 {
        let delay = schedule.after(&rejected(), &config(), 0);
        assert!(delay <= Duration::from_secs(86400));
    }
}

#[test]
fn ack_resets_the_backoff() {
    let mut schedule = Schedule::new();
    schedule.after(&rejected(), &config(), 0);
    schedule.after(&rejected(), &config(), 0);
    schedule.after(&acked(), &config(), 0);
    let delay = schedule.after(&rejected(), &config(), 0);
    assert_eq!(delay, Duration::from_secs(2 * 3600));
}
