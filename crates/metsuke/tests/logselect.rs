//! Selection against the recorded trace stream. The recordings are contiguous
//! windows of one node's stdout, so a rule tested here faces every trace the
//! node emitted alongside the wanted ones (tests/fixtures/README.md).

use metsuke::logselect::{self, SelectConfig, Selection, Severity, select};

mod support;
use support::{shipped_log_config, shipped_rules};

const LEIOS_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces.log");
const STARTUP_WINDOW: &str = include_str!("fixtures/recordings/leios-node-traces-startup.log");

fn line_with(window: &'static str, needle: &str) -> &'static str {
    window
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("the recording holds no {needle} line"))
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
            Selection::Ship(line),
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
    assert_eq!(select(&rules, warning), Selection::Ship(warning));
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
    assert_eq!(select(&rules, debug), Selection::Ship(debug));
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
    let namespace = logselect::namespace(warning).unwrap();
    assert!(
        !shipped_log_config()
            .namespace_roots
            .iter()
            .any(|root| namespace.starts_with(root)),
        "{namespace} is under a root, so this proves nothing"
    );
    assert_eq!(select(&rules, warning), Selection::Ship(warning));
}

// The node prints its whole configuration before the tracing system is up. It
// is the only line in the recorded windows that is not a trace record, and
// neither rule can reach it because it declares no namespace and no severity.
#[test]
fn the_pre_tracing_configuration_dump_is_skipped() {
    let first = STARTUP_WINDOW.lines().next().unwrap();
    assert!(first.starts_with("Node configuration:"), "{first:.60}");
    assert_eq!(logselect::namespace(first), None);
    assert_eq!(logselect::severity(first), None);
    assert_eq!(select(&shipped_rules(), first), Selection::Skip);
}

// What the two string rules rest on: the node writes `ns` before `data` and
// `sev` after it, with only flat keys following. So the first `ns` and the
// last `sev` in a line are the line's own, whatever `data` nests. The
// recording is the guard — a node that reorders its keys makes what `select`
// reads and what the line says two different things, and this is where that
// shows.
#[test]
fn the_string_rules_read_what_a_json_parse_reads() {
    let mut records = 0;
    let mut not_json = 0;
    for line in LEIOS_WINDOW.lines().chain(STARTUP_WINDOW.lines()) {
        let Ok(parsed) = serde_json::from_str::<serde_json::Value>(line) else {
            not_json += 1;
            continue;
        };
        records += 1;
        assert_eq!(
            logselect::namespace(line),
            parsed["ns"].as_str(),
            "{line:.120}"
        );
        let sev = parsed["sev"]
            .as_str()
            .unwrap_or_else(|| panic!("a trace record declares a severity: {line:.120}"));
        assert_eq!(
            logselect::severity(line),
            Some(sev.parse().expect("the ladder covers what the node writes")),
            "{line:.120}"
        );
    }
    assert!(records > 0, "the recordings hold no trace records");
    // The pre-tracing configuration dump, and nothing else.
    assert_eq!(not_json, 1);
}

// The point of selecting at all. Stated as a shape rather than a ratio: the
// shipped rules select something, and nothing they select is a Debug line —
// which is where the volume in this recording is, and none of it was asked
// for.
#[test]
fn the_shipped_rules_select_without_reaching_debug() {
    let rules = shipped_rules();
    let shipped: Vec<&str> = LEIOS_WINDOW
        .lines()
        .filter(|line| matches!(select(&rules, line), Selection::Ship(_)))
        .collect();
    assert!(!shipped.is_empty(), "the rules selected nothing at all");
    assert!(shipped.len() < LEIOS_WINDOW.lines().count());
    let debug: Vec<&&str> = shipped
        .iter()
        .filter(|line| line.contains(r#""sev":"Debug""#))
        .collect();
    assert!(debug.is_empty(), "{} Debug lines selected", debug.len());
}

// The line that goes to the spool is the node's own bytes, not a
// re-serialization: metsuke does not know what a Leios trace means, and the
// developers compute their own distributions from the raw record.
#[test]
fn a_shipped_line_is_the_original_bytes() {
    let line = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosKernel.Certified""#);
    let Selection::Ship(shipped) = select(&shipped_rules(), line) else {
        panic!("the shipped rules dropped a Certified line");
    };
    assert!(shipped.as_ptr() == line.as_ptr() && shipped.len() == line.len());
}
