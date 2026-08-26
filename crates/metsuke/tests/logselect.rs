//! Selection against the recorded trace stream. The recordings are contiguous
//! windows of one node's stdout, so a rule tested here faces every trace the
//! node emitted alongside the wanted ones (tests/fixtures/README.md).

use metsuke::logselect::{Fields, SelectConfig, Selection, Severity, select};
use metsuke_wire::envelope::TraceLine;

mod support;
use support::{shipped_log_config, shipped_rules, trace_line};

const LEIOS_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces.log");
const STARTUP_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces-startup.log");

fn line_with(window: &'static str, needle: &str) -> &'static str {
    window
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("the recording holds no {needle} line"))
}

/// What selecting `line` looks like when a rule keeps it.
fn ship(line: &str) -> Selection {
    Selection::Ship(trace_line(line))
}

// What the rewards program asked for, by the namespaces a node actually emits.
// Every one of these is severity Info, below the Notice floor, so the
// namespace rule is the only thing that can select them.
#[test]
fn the_shipped_rules_ship_what_was_asked_for() {
    let rules = shipped_rules();
    for needle in [
        r#""ns":"Consensus.LeiosPeer.Announcement""#,
        r#""ns":"Consensus.LeiosKernel.BlockAcquired""#,
        r#""ns":"Consensus.LeiosKernel.BlockTxsAcquired""#,
        r#""ns":"Consensus.LeiosKernel.Certified""#,
        r#""ns":"Consensus.LeiosKernel.NotVoted""#,
        r#""ns":"ChainDB.AddBlockEvent.AddedToCurrentChain""#,
        r#""ns":"Forge.Loop.AdoptedBlock""#,
    ] {
        let line = line_with(LEIOS_WINDOW, needle);
        assert_eq!(
            select(&rules, line),
            ship(line),
            "the shipped rules dropped {needle}"
        );
    }
}

// The loudest namespace in the recording is wire-level keepalive polling that
// nobody asked for, and it is Debug, so neither rule reaches it.
#[test]
fn the_shipped_rules_drop_the_wire_level_chatter() {
    let rules = shipped_rules();
    for needle in [
        r#""ns":"LeiosNotify.Remote.Send.RequestNext""#,
        r#""ns":"Forge.Loop.Call""#,
        r#""ns":"ChainDB.LedgerEvent.Flavor.V2.LedgerTablesHandleCreate""#,
    ] {
        assert_eq!(
            select(&rules, line_with(LEIOS_WINDOW, needle)),
            Selection::Skip,
            "the shipped rules kept {needle}"
        );
    }
}

// The severity rule answers "every error, warning and notice trace" on its
// own, whatever the namespace: a Warning outside every configured prefix is
// still shipped.
#[test]
fn the_severity_rule_reaches_past_the_namespace_list() {
    let rules = SelectConfig::new(&[], vec![], Severity::Warning).unwrap();
    let warning = line_with(STARTUP_WINDOW, r#""sev":"Warning""#);
    assert_eq!(select(&rules, warning), ship(warning));
    assert_eq!(
        select(&rules, line_with(LEIOS_WINDOW, r#""sev":"Info""#)),
        Selection::Skip
    );
}

// A namespace prefix with no severity floor under it: the two rules are
// independent, and asking for one namespace does not drag a severity in.
#[test]
fn the_namespace_rule_reaches_below_the_severity_floor() {
    let rules = SelectConfig::new(
        &["LeiosNotify.".to_string()],
        vec!["LeiosNotify.Remote".to_string()],
        Severity::Emergency,
    )
    .unwrap();
    let debug = line_with(
        LEIOS_WINDOW,
        r#""ns":"LeiosNotify.Remote.Send.RequestNext""#,
    );
    assert_eq!(select(&rules, debug), ship(debug));
}

// The ceiling metsuke-4zo.99 will check a server-pushed rule against. It binds
// today too: a namespace nobody put under a root is refused where the rules are
// built, not carried as one that is honoured.
#[test]
fn a_namespace_outside_the_roots_is_refused() {
    let error = SelectConfig::new(
        &shipped_log_config().namespace_roots,
        vec!["Consensus.Leios".to_string(), "Reflection".to_string()],
        Severity::Notice,
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Reflection"),
        "the refusal must name the namespace, got: {error}"
    );
}

// The severity floor is not under the ceiling: "every error, warning and notice
// trace" is an ask about the node's whole namespace tree, and the recording's
// startup Warnings sit outside every shipped root.
#[test]
fn the_roots_do_not_bound_the_severity_rule() {
    let rules = shipped_rules();
    let warning = line_with(STARTUP_WINDOW, r#""sev":"Warning""#);
    let parsed = trace_line(warning);
    let namespace = Fields::of(&parsed).namespace.unwrap();
    assert!(
        !shipped_log_config()
            .namespace_roots
            .iter()
            .any(|root| namespace.starts_with(root)),
        "{namespace} is under a root, so this proves nothing"
    );
    assert_eq!(select(&rules, warning), ship(warning));
}

// The node prints its whole configuration before the tracing system is up. It
// is the only line in the recorded windows that is not a trace record, and
// neither rule can reach it because it is not a JSON object at all.
#[test]
fn the_pre_tracing_configuration_dump_is_skipped() {
    let first = STARTUP_WINDOW.lines().next().unwrap();
    assert!(first.starts_with("Node configuration:"), "{first:.60}");
    assert!(TraceLine::parse(first).is_err());
    assert_eq!(select(&shipped_rules(), first), Selection::Skip);
}

// A line cut short is not a JSON object, so it declares nothing and no rule
// reaches it — not even the rule that selected the whole line, and the cut
// here keeps both fields intact.
#[test]
fn a_truncated_line_declares_nothing_and_is_not_selected() {
    let line = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosKernel.Certified""#);
    assert_eq!(select(&shipped_rules(), line), ship(line));
    let cut = &line[..line.rfind(',').unwrap()];
    assert!(cut.contains(r#""ns":"Consensus.LeiosKernel.Certified""#));
    assert!(TraceLine::parse(cut).is_err());
    assert_eq!(select(&shipped_rules(), cut), Selection::Skip);
}

// `data` is a namespace's own payload and the node puts whatever the trace
// carries in it, `ns` and `sev` included. This is the case the substring
// readers needed a first-occurrence and a last-occurrence rule to survive.
#[test]
fn a_namespace_nested_in_data_is_not_the_lines_namespace() {
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:56Z","ns":"Forge.Loop.AdoptedBlock","data":{"ns":"LeiosNotify.Remote.Send.RequestNext","sev":"Debug"},"sev":"Info","thread":"52","host":"alpha"}"#,
    );
    assert_eq!(
        Fields::of(&line),
        Fields {
            namespace: Some("Forge.Loop.AdoptedBlock"),
            severity: Some(Severity::Info),
        }
    );
}

// A field the line does not spell literally still reads: the rules see the
// value, not the bytes the node escaped it into.
#[test]
fn a_field_reads_through_its_escapes() {
    let escaped = trace_line(r#"{"ns":"Forge\u002ELoop","sev":"Info"}"#);
    assert_eq!(Fields::of(&escaped).namespace, Some("Forge.Loop"));
}

// Every record in the recordings declares both fields. A node that stops
// declaring one, or spells a severity this build does not know, silently
// loses the rule that reads it — this is where that shows, and the count of
// lines declaring neither is what holds the pre-tracing dump to being the only
// one.
#[test]
fn every_recorded_record_declares_the_fields_a_rule_reads() {
    let mut records = 0;
    let mut declaring_neither = 0;
    for line in LEIOS_WINDOW.lines().chain(STARTUP_WINDOW.lines()) {
        let Ok(parsed) = TraceLine::parse(line) else {
            declaring_neither += 1;
            continue;
        };
        let fields = Fields::of(&parsed);
        if fields == Fields::default() {
            declaring_neither += 1;
            continue;
        }
        records += 1;
        assert!(fields.namespace.is_some(), "{line:.120}");
        assert!(fields.severity.is_some(), "{line:.120}");
    }
    assert!(records > 0, "the recordings hold no trace records");
    // The pre-tracing configuration dump, and nothing else.
    assert_eq!(declaring_neither, 1);
}

// The point of selecting at all. Stated as a shape rather than a ratio: the
// shipped rules select something, and nothing they select is a Debug line —
// which is where the volume in this recording is, and none of it was asked
// for.
#[test]
fn the_shipped_rules_select_without_reaching_debug() {
    let rules = shipped_rules();
    let shipped: Vec<TraceLine> = LEIOS_WINDOW
        .lines()
        .filter_map(|line| match select(&rules, line) {
            Selection::Ship(line) => Some(line),
            Selection::Skip => None,
        })
        .collect();
    assert!(!shipped.is_empty(), "the rules selected nothing at all");
    assert!(shipped.len() < LEIOS_WINDOW.lines().count());
    let debug: Vec<&TraceLine> = shipped
        .iter()
        .filter(|line| Fields::of(line).severity == Some(Severity::Debug))
        .collect();
    assert!(debug.is_empty(), "{} Debug lines selected", debug.len());
}

// The line that goes to the spool is every field the node wrote, under the keys
// it wrote them: metsuke does not know what a Leios trace means, and the
// developers compute their own distributions from the record's own fields.
#[test]
fn a_shipped_line_holds_every_field_the_node_wrote() {
    let line = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosKernel.Certified""#);
    let Selection::Ship(shipped) = select(&shipped_rules(), line) else {
        panic!("the shipped rules dropped a Certified line");
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&shipped.to_line()).unwrap(),
        serde_json::from_str::<serde_json::Value>(line).unwrap()
    );
}
