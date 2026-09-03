//! Upload scheduling tests (ticket metsuke-4zo.5): outcome in, delay until
//! the next attempt out. 5xx/transport and 429 retry next interval with
//! jitter; the other 4xx back off exponentially with a clamp; an ack resets
//! the backoff.

use std::time::Duration;

use metsuke::schedule::{Schedule, ScheduleConfig, next_upload_line};
use metsuke::uploader::UploadOutcome;
use metsuke_wire::envelope::Ack;
use time::OffsetDateTime;

// What a tick says once it has sent what it had: when the next one is,
// which an operator reading a journal cannot work out, because jitter placed
// this agent inside the interval and a refusal replaces it with a backoff.
#[test]
fn the_line_a_tick_ends_on_names_when_the_next_one_is() {
    let now = OffsetDateTime::from_unix_timestamp(1_780_000_000).unwrap();

    assert_eq!(
        next_upload_line(now, Duration::from_secs(3540)),
        "the next submission is scheduled at 2026-05-28T21:25:40Z"
    );
}

// Subsecond digits are the clock's, not an interval an operator set, so the
// instant is named to the second.
#[test]
fn the_instant_is_named_to_the_second() {
    let now = OffsetDateTime::from_unix_timestamp_nanos(1_780_000_000_123_456_789).unwrap();

    let line = next_upload_line(now, Duration::from_secs(60));

    assert!(line.contains("T20:27:40Z"), "got: {line}");
}

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

// The first accepted submission places this agent somewhere in the interval.
// Without it, agents installed in one window upload in the same second of
// every interval for as long as they run, and nothing in a fleet that is
// working ever breaks the step.
#[test]
fn the_first_ack_places_the_agent_within_the_jitter_bound() {
    let interval = Duration::from_secs(3600);
    let jitter_max = Duration::from_secs(60);
    for entropy in [0, 1, 59, 60, 61, u64::MAX] {
        let mut schedule = Schedule::new();
        let delay = schedule.after(&acked(), &config(), entropy);
        assert!(
            (interval..=interval + jitter_max).contains(&delay),
            "entropy {entropy}: delay {delay:?} outside [interval, interval + jitter_max]"
        );
    }
}

// And that placement has to actually differ between agents, or it is
// decorative.
#[test]
fn two_agents_starting_together_are_placed_apart() {
    let placed: Vec<Duration> = (0..10)
        .map(|entropy| Schedule::new().after(&acked(), &config(), entropy))
        .collect();
    assert!(
        placed.windows(2).any(|pair| pair[0] != pair[1]),
        "ten entropies, one delay: {placed:?}"
    );
}

// Every upload after that one is the interval exactly. An operator watching a
// working agent sees a submission on a cadence they can predict, rather than
// one that walks around the clock; the spread is a placement, not a wobble.
#[test]
fn every_ack_after_the_first_schedules_the_exact_interval() {
    let mut schedule = Schedule::new();
    schedule.after(&acked(), &config(), 41);
    let later: Vec<Duration> = (0..5)
        .map(|entropy| schedule.after(&acked(), &config(), entropy))
        .collect();
    assert!(
        later
            .iter()
            .all(|delay| *delay == Duration::from_secs(3600)),
        "the cadence must not drift once placed, got {later:?}"
    );
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
