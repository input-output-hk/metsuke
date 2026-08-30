//! What the scrape tick says and how often (ticket metsuke-09v0). The policy
//! is pure, so nothing here runs a scrape: news in, journal lines out.

use metsuke::agent::ScrapeNews;
use metsuke::report::ScrapeReport;
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

// A scrape with nothing to report is silent, which is every scrape against a
// node that is up.
#[test]
fn a_clean_scrape_says_nothing() {
    let mut report = ScrapeReport::default();
    assert_eq!(report.record(&clean()), Vec::<String>::new());
    assert_eq!(report.drain(), Vec::<String>::new());
}

// The point of the ticket: the first failure is named at once, and the ones
// behind it do not each write the same line.
#[test]
fn only_the_first_failure_of_a_run_is_named() {
    let mut report = ScrapeReport::default();
    let first = report.record(&failed("connection refused"));
    assert_eq!(first.len(), 1, "the first failure is named as it happens");
    assert!(first[0].contains("connection refused"), "{first:?}");
    for _ in 0..9 {
        assert_eq!(
            report.record(&failed("connection refused")),
            Vec::<String>::new()
        );
    }
}

// And what they added up to is said once, on the boundary the upload tick
// sets, so a node that stays down is still heard from at that cadence.
#[test]
fn the_failures_behind_the_first_are_counted_and_said_once() {
    let mut report = ScrapeReport::default();
    for _ in 0..10 {
        report.record(&failed("connection refused"));
    }
    let drained = report.drain();
    assert_eq!(drained.len(), 1, "{drained:?}");
    assert!(drained[0].contains("10 scrapes have failed"), "{drained:?}");
}

// A drain resets the run, so the next failure is named as it happens again
// rather than the endpoint going quiet for the rest of the process.
#[test]
fn a_drained_report_names_the_next_failure_again() {
    let mut report = ScrapeReport::default();
    report.record(&failed("connection refused"));
    report.record(&failed("connection refused"));
    report.drain();
    assert_eq!(report.record(&failed("connection refused")).len(), 1);
}

// One failure needs no summary: the line that named it already said so.
#[test]
fn a_single_failure_is_not_restated_on_the_boundary() {
    let mut report = ScrapeReport::default();
    assert_eq!(report.record(&failed("connection refused")).len(), 1);
    assert_eq!(report.drain(), Vec::<String>::new());
}

// Refused lines are the same policy on their own counter, and the summary
// carries both numbers: how many lines, across how many scrapes.
#[test]
fn refused_lines_are_named_once_then_counted_across_scrapes() {
    let mut report = ScrapeReport::default();
    let first = report.record(&refused(3));
    assert_eq!(first.len(), 1, "{first:?}");
    assert!(
        first[0].starts_with("3 of the node's metric lines"),
        "{first:?}"
    );
    assert_eq!(report.record(&refused(4)), Vec::<String>::new());
    let drained = report.drain();
    assert_eq!(drained.len(), 1, "{drained:?}");
    assert!(
        drained[0].contains("7 of the node's metric lines"),
        "{drained:?}"
    );
    assert!(drained[0].contains("across 2 scrapes"), "{drained:?}");
}

// The two counters are apart: a body that both failed to fetch and held bad
// lines cannot happen, but a run of each interleaved must still summarise both
// without either resetting the other.
#[test]
fn the_two_counters_are_reported_apart() {
    let mut report = ScrapeReport::default();
    for _ in 0..2 {
        report.record(&failed("connection refused"));
        report.record(&refused(1));
    }
    let drained = report.drain();
    assert_eq!(drained.len(), 2, "{drained:?}");
    assert!(drained[0].contains("2 scrapes have failed"), "{drained:?}");
    assert!(drained[1].contains("across 2 scrapes"), "{drained:?}");
}
