//! What the scrape tick says and how often (ticket metsuke-09v0). The policy
//! is pure, so nothing here runs a scrape: news in, journal lines out.

use metsuke::agent::ScrapeNews;
use metsuke::report::{Line, ScrapeReport};
use metsuke::scrape::Refused;

fn failed(detail: &str) -> ScrapeNews {
    ScrapeNews {
        failed: Some(detail.to_string()),
        refused: Vec::new(),
    }
}

fn refused(lines: usize) -> ScrapeNews {
    ScrapeNews {
        failed: None,
        refused: (0..lines)
            .map(|index| Refused::Unreadable {
                line: format!("line {index}"),
            })
            .collect(),
    }
}

fn clean() -> ScrapeNews {
    ScrapeNews::default()
}

/// The tick past the startup window, which is every tick of a run whose node
/// was already up.
fn running(report: &mut ScrapeReport, news: &ScrapeNews) -> Vec<Line> {
    report.record(news, false)
}

/// What a line says, for the assertions about wording rather than severity.
fn text(line: &Line) -> &str {
    match line {
        Line::Info(text) | Line::Warning(text) => text,
    }
}

// A scrape with nothing to report is silent, which is every scrape against a
// node that is up.
#[test]
fn a_clean_scrape_says_nothing() {
    let mut report = ScrapeReport::default();
    assert_eq!(running(&mut report, &clean()), Vec::<Line>::new());
    assert_eq!(report.drain(), Vec::<Line>::new());
}

// The point of the ticket: the first failure is named at once, and the ones
// behind it do not each write the same line.
#[test]
fn only_the_first_failure_of_a_run_is_named() {
    let mut report = ScrapeReport::default();
    let first = running(&mut report, &failed("connection refused"));
    assert_eq!(first.len(), 1, "the first failure is named as it happens");
    assert!(text(&first[0]).contains("connection refused"), "{first:?}");
    for _ in 0..9 {
        assert_eq!(
            running(&mut report, &failed("connection refused")),
            Vec::<Line>::new()
        );
    }
}

// A node that has answered and then stopped is a fault, and that is what a
// warning is for.
#[test]
fn a_failure_against_a_node_that_was_up_is_a_warning() {
    let mut report = ScrapeReport::default();
    let named = running(&mut report, &failed("connection refused"));
    assert!(matches!(named[0], Line::Warning(_)), "{named:?}");
}

// In pipe mode the node comes up alongside the agent, so the first scrape
// meeting nothing is news rather than a fault. It still ships as a failure,
// which is why the line says so.
#[test]
fn a_failure_before_the_node_has_ever_answered_is_not_a_warning() {
    let mut report = ScrapeReport::default();

    let named = report.record(&failed("connection refused"), true);

    assert!(matches!(named[0], Line::Info(_)), "{named:?}");
    assert!(
        text(&named[0]).contains("has not answered yet"),
        "{named:?}"
    );
    assert!(text(&named[0]).contains("connection refused"), "{named:?}");
}

// And once it has answered, a failure inside that same window is a fault like
// any other: the node was there and went away.
#[test]
fn a_failure_after_the_node_answered_is_a_warning_even_while_starting() {
    let mut report = ScrapeReport::default();
    report.record(&clean(), true);

    let named = report.record(&failed("connection refused"), true);

    assert!(matches!(named[0], Line::Warning(_)), "{named:?}");
}

// The recovery an operator would otherwise only learn about from an upload an
// interval later.
#[test]
fn the_scrape_that_answers_after_failures_says_so() {
    let mut report = ScrapeReport::default();
    for _ in 0..3 {
        running(&mut report, &failed("connection refused"));
    }

    let recovered = running(&mut report, &clean());

    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert!(matches!(recovered[0], Line::Info(_)), "{recovered:?}");
    assert!(text(&recovered[0]).contains("answered"), "{recovered:?}");
    assert!(text(&recovered[0]).contains("3 scrapes"), "{recovered:?}");
}

// Said once, not on every scrape after it.
#[test]
fn the_scrapes_after_a_recovery_are_silent_again() {
    let mut report = ScrapeReport::default();
    running(&mut report, &failed("connection refused"));
    running(&mut report, &clean());

    assert_eq!(running(&mut report, &clean()), Vec::<Line>::new());
}

// The upload boundary zeroes what a summary is of, and the recovery line is
// not one of those: an endpoint that goes down and comes back across a
// boundary still has to be able to say what it missed.
#[test]
fn a_recovery_across_an_upload_boundary_still_counts_the_failures() {
    let mut report = ScrapeReport::default();
    running(&mut report, &failed("connection refused"));
    report.drain();

    let recovered = running(&mut report, &clean());

    assert_eq!(recovered.len(), 1, "{recovered:?}");
    assert!(text(&recovered[0]).contains("one scrape"), "{recovered:?}");
}

// What the loop waits on before it settles into the configured interval.
#[test]
fn the_report_says_whether_the_endpoint_has_ever_answered() {
    let mut report = ScrapeReport::default();
    assert!(!report.answered());

    running(&mut report, &failed("connection refused"));
    assert!(!report.answered());

    running(&mut report, &clean());
    assert!(report.answered());
}

// And what they added up to is said once, on the boundary the upload tick
// sets, so a node that stays down is still heard from at that cadence.
#[test]
fn the_failures_behind_the_first_are_counted_and_said_once() {
    let mut report = ScrapeReport::default();
    for _ in 0..10 {
        running(&mut report, &failed("connection refused"));
    }
    let drained = report.drain();
    assert_eq!(drained.len(), 1, "{drained:?}");
    assert!(
        text(&drained[0]).contains("10 scrapes have failed"),
        "{drained:?}"
    );
}

// A drain resets the run, so the next failure is named as it happens again
// rather than the endpoint going quiet for the rest of the process.
#[test]
fn a_drained_report_names_the_next_failure_again() {
    let mut report = ScrapeReport::default();
    running(&mut report, &failed("connection refused"));
    running(&mut report, &failed("connection refused"));
    report.drain();
    assert_eq!(running(&mut report, &failed("connection refused")).len(), 1);
}

// One failure needs no summary: the line that named it already said so.
#[test]
fn a_single_failure_is_not_restated_on_the_boundary() {
    let mut report = ScrapeReport::default();
    assert_eq!(running(&mut report, &failed("connection refused")).len(), 1);
    assert_eq!(report.drain(), Vec::<Line>::new());
}

// Refused lines are the same policy on their own counter, and the summary
// carries both numbers: how many lines, across how many scrapes.
#[test]
fn refused_lines_are_named_once_then_counted_across_scrapes() {
    let mut report = ScrapeReport::default();
    let first = running(&mut report, &refused(3));
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        text(&first[0]).starts_with("3 of the node's metric lines"),
        "{first:?}"
    );
    assert_eq!(running(&mut report, &refused(4)), Vec::<Line>::new());
    let drained = report.drain();
    assert_eq!(drained.len(), 1, "{drained:?}");
    assert!(
        text(&drained[0]).contains("7 of the node's metric lines"),
        "{drained:?}"
    );
    assert!(
        text(&drained[0]).contains("across 2 scrapes"),
        "{drained:?}"
    );
}

// The two counters are apart: a body that both failed to fetch and held bad
// lines cannot happen, but a run of each interleaved must still summarise both
// without either resetting the other.
#[test]
fn the_two_counters_are_reported_apart() {
    let mut report = ScrapeReport::default();
    for _ in 0..2 {
        running(&mut report, &failed("connection refused"));
        running(&mut report, &refused(1));
    }
    let drained = report.drain();
    assert_eq!(drained.len(), 2, "{drained:?}");
    assert!(
        text(&drained[0]).contains("2 scrapes have failed"),
        "{drained:?}"
    );
    assert!(
        text(&drained[1]).contains("across 2 scrapes"),
        "{drained:?}"
    );
}
