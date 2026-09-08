//! Selection against the recorded trace stream. The recordings are contiguous
//! windows of one node's stdout, so a rule tested here faces every trace the
//! node emitted alongside the wanted ones (tests/fixtures/README.md).

use metsuke::logselect::{Fields, SelectConfig, Selection, select};
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

/// The line's own `sev`. Nothing in the agent reads it. These tests do, to say
/// which lines the namespace rule reaches without consulting it. A record that
/// declares none fails here rather than reading as "not the severity I asked
/// about", which would let a recording without `sev` pass every caller.
fn severity_of(line: &TraceLine) -> String {
    line.get("sev")
        .and_then(serde_json::Value::as_str)
        .expect("a recorded trace record declares sev")
        .to_string()
}

// What the rewards program asked for, by the namespaces a node actually emits.
#[test]
fn the_shipped_rules_ship_what_was_asked_for() {
    let rules = shipped_rules();
    for needle in [
        r#""ns":"Consensus.LeiosPeer.Announcement""#,
        r#""ns":"Consensus.LeiosKernel.BlockAcquired""#,
        r#""ns":"Consensus.LeiosKernel.BlockTxsAcquired""#,
        r#""ns":"Consensus.LeiosKernel.Certified""#,
        // The vote path as a round that went well leaves it. Its counterpart
        // `NotVoted` is a node declining to vote, which a devnet where nothing
        // goes wrong never emits, so a recording is not the place to ask for
        // one.
        r#""ns":"Consensus.LeiosKernel.Voted""#,
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

// Lines no rule names. The recording's loudest namespaces are the two Leios
// `Msg` streams, which are wanted, so these are picked for the shape of how
// they reach the rules rather than for how much of the window they are.
#[test]
fn the_shipped_rules_drop_the_wire_level_chatter() {
    let rules = shipped_rules();
    for (window, needle) in [
        // Outside the roots, under a selected root beside a namespace that is
        // wanted, and under one on a branch of its own: the three ways a line
        // reaches these rules without being asked for.
        (
            LEIOS_WINDOW,
            r#""ns":"LeiosNotify.Remote.Send.RequestNext""#,
        ),
        (LEIOS_WINDOW, r#""ns":"Forge.Loop.StartLeadershipCheck""#),
        (LEIOS_WINDOW, r#""ns":"Forge.StateInfo.StateInfo""#),
        // The third root, from the other window because that is where it says
        // anything: what ChainDB emits around a Leios round is the one
        // namespace that is wanted, its Debug ledger-table traces are under
        // the Info the deployed configuration holds the root to, and the rest
        // of it is the database opening.
        (STARTUP_WINDOW, r#""ns":"ChainDB.OpenEvent.OpenedDB""#),
    ] {
        assert_eq!(
            select(&rules, line_with(window, needle)),
            Selection::Skip,
            "the shipped rules kept {needle}"
        );
    }
}

// The namespace list is the whole rule: a Warning under no listed namespace is
// dropped like anything else there.
#[test]
fn a_warning_outside_the_namespaces_is_dropped() {
    let warning = line_with(STARTUP_WINDOW, r#""sev":"Warning""#);
    // The needle is a substring and `data` carries the same keys, so the match
    // is only a Warning at the top level if this says so.
    assert_eq!(severity_of(&trace_line(warning)), "Warning");
    assert_eq!(select(&shipped_rules(), warning), Selection::Skip);
}

// The other direction: a listed namespace is shipped at whatever severity its
// lines carry, Debug included.
#[test]
fn a_listed_namespace_ships_at_any_severity() {
    let rules = SelectConfig::new(
        &["LeiosNotify".to_string()],
        vec!["LeiosNotify.Remote".to_string()],
        Vec::new(),
    )
    .unwrap();
    let debug = line_with(
        LEIOS_WINDOW,
        r#""ns":"LeiosNotify.Remote.Send.RequestNext""#,
    );
    assert_eq!(severity_of(&trace_line(debug)), "Debug");
    assert_eq!(select(&rules, debug), ship(debug));
}

// A rule that stops mid-segment names no namespace, so it selects nothing,
// which is what the shipped default did before metsuke-4zo.107 named the two
// prefixes the node emits.
#[test]
fn a_rule_stopping_mid_segment_selects_nothing() {
    let rules = SelectConfig::new(
        &shipped_log_config().namespace_roots,
        vec!["Consensus.Leios".to_string()],
        Vec::new(),
    )
    .unwrap();
    let kernel = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosKernel.Certified""#);
    assert_eq!(select(&rules, kernel), Selection::Skip);
    assert!(
        LEIOS_WINDOW
            .lines()
            .all(|line| select(&rules, line) == Selection::Skip)
    );
}

// The ceiling metsuke-4zo.99 will check a server-pushed rule against. It binds
// today too: a namespace nobody put under a root is refused where the rules are
// built, not carried as one that is honoured.
#[test]
fn a_namespace_outside_the_roots_is_refused() {
    let error = SelectConfig::new(
        &shipped_log_config().namespace_roots,
        vec!["Consensus.Leios".to_string(), "Reflection".to_string()],
        Vec::new(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Reflection"),
        "the refusal must name the namespace, got: {error}"
    );
    // The ceiling matches by segment too: a name a root is a prefix of
    // mid-segment is outside it.
    assert!(
        SelectConfig::new(
            &shipped_log_config().namespace_roots,
            vec!["ConsensusOther.Thing".to_string()],
            Vec::new(),
        )
        .is_err()
    );
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

// A node line that happens to hold the reserved provenance key at its top
// level is well-formed, not malformed, so it must not fall into the same
// bucket as a line that declares nothing (metsuke-jfb.12).
#[test]
fn a_line_declaring_the_reserved_key_is_not_skip() {
    let line = r#"{"ns":"Forge.Loop.AdoptedBlock","sev":"Info","metsuke":1}"#;
    assert_eq!(select(&shipped_rules(), line), Selection::ReservedKey);
}

// A line cut short is not a JSON object, so it declares nothing and no rule
// reaches it, not even the rule that selected the whole line, and the cut
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
// carries in it, `ns` included. This is the case the substring readers needed a
// first-occurrence rule to survive.
#[test]
fn a_namespace_nested_in_data_is_not_the_lines_namespace() {
    let line = trace_line(
        r#"{"at":"2026-08-25T18:19:56Z","ns":"Forge.Loop.AdoptedBlock","data":{"ns":"LeiosNotify.Remote.Send.RequestNext","sev":"Debug"},"sev":"Info","thread":"52","host":"alpha"}"#,
    );
    assert_eq!(
        Fields::of(&line),
        Fields {
            namespace: Some("Forge.Loop.AdoptedBlock"),
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

// Every record in the recordings declares the one field a rule reads. A node
// that stops declaring it silently loses every rule. This is where that shows,
// and the count of lines declaring nothing is what holds the pre-tracing dump to
// being the only one.
#[test]
fn every_recorded_record_declares_the_field_a_rule_reads() {
    let mut records = 0;
    let mut declaring_nothing = 0;
    for line in LEIOS_WINDOW.lines().chain(STARTUP_WINDOW.lines()) {
        let Ok(parsed) = TraceLine::parse(line) else {
            declaring_nothing += 1;
            continue;
        };
        match Fields::of(&parsed).namespace {
            None => declaring_nothing += 1,
            Some(_) => records += 1,
        }
    }
    assert!(records > 0, "the recordings hold no trace records");
    // The pre-tracing configuration dump, and nothing else.
    assert_eq!(declaring_nothing, 1);
}

// The point of selecting at all. Stated as a shape rather than a ratio: the
// shipped rules select something, and nothing they select is a Debug line. A
// third of this window is Debug and none of it was asked for, so what the
// assertion is worth is that the rules reach none of it.
#[test]
fn the_shipped_rules_select_without_reaching_debug() {
    let rules = shipped_rules();
    let shipped: Vec<TraceLine> = LEIOS_WINDOW
        .lines()
        .filter_map(|line| match select(&rules, line) {
            Selection::Ship(line) => Some(line),
            Selection::Skip | Selection::ReservedKey => None,
        })
        .collect();
    assert!(!shipped.is_empty(), "the rules selected nothing at all");
    assert!(shipped.len() < LEIOS_WINDOW.lines().count());
    let debug = shipped
        .iter()
        .filter(|line| severity_of(line) == "Debug")
        .count();
    assert_eq!(debug, 0, "{debug} Debug lines selected");
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

/// The case the feature is for, against the recording rather than a synthetic
/// line: `Consensus.LeiosPeer.Msg` is two thirds of what a node ships and
/// nothing downstream can group by it, while `Consensus.LeiosPeer.Announcement`
/// under the same prefix answers one of the distributions the program asked
/// for. An exclusion keeps the prefix and drops the one namespace.
#[test]
fn an_exclusion_drops_a_namespace_under_a_prefix_that_is_kept() {
    let log = shipped_log_config();
    let rules = SelectConfig::new(
        &log.namespace_roots,
        log.namespaces.clone(),
        vec!["Consensus.LeiosPeer.Msg".to_string()],
    )
    .unwrap();

    let excluded = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosPeer.Msg""#);
    assert_eq!(select(&rules, excluded), Selection::Skip);

    // The sibling under the same prefix is untouched, which is the whole
    // difference between this and narrowing `namespaces`.
    let kept = line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosKernel.Msg""#);
    assert!(
        matches!(select(&rules, kept), Selection::Ship(_)),
        "an exclusion took a namespace it does not name"
    );

    // And with no exclusion the same line ships, so the test is about the rule
    // rather than about the recording.
    assert!(
        matches!(select(&shipped_rules(), excluded), Selection::Ship(_)),
        "the shipped rules already dropped this line, so nothing was excluded"
    );
}

/// An exclusion is a prefix like any other rule, so it takes the namespaces
/// under it and stops at a segment boundary rather than at shared letters.
#[test]
fn an_exclusion_matches_on_segment_boundaries() {
    let log = shipped_log_config();
    let whole_prefix = SelectConfig::new(
        &log.namespace_roots,
        log.namespaces.clone(),
        vec!["Consensus.LeiosPeer".to_string()],
    )
    .unwrap();
    for needle in [
        r#""ns":"Consensus.LeiosPeer.Msg""#,
        r#""ns":"Consensus.LeiosPeer.Announcement""#,
    ] {
        assert_eq!(
            select(&whole_prefix, line_with(LEIOS_WINDOW, needle)),
            Selection::Skip,
            "{needle} survived an exclusion of its parent"
        );
    }

    // Mid-segment, so it names nothing the node emits and excludes nothing.
    let fragment = SelectConfig::new(
        &log.namespace_roots,
        log.namespaces.clone(),
        vec!["Consensus.LeiosP".to_string()],
    )
    .unwrap();
    assert!(
        matches!(
            select(
                &fragment,
                line_with(LEIOS_WINDOW, r#""ns":"Consensus.LeiosPeer.Msg""#)
            ),
            Selection::Ship(_)
        ),
        "a rule stopping mid-segment excluded whatever shares its letters"
    );
}

/// Exclusions are not held to the ceiling: they only ever ship less, so one
/// naming a namespace no root covers is redundant rather than wrong.
#[test]
fn an_exclusion_outside_the_roots_is_allowed() {
    let log = shipped_log_config();
    SelectConfig::new(
        &log.namespace_roots,
        log.namespaces.clone(),
        vec!["Reflection".to_string()],
    )
    .expect("an exclusion outside the roots narrows nothing and is not refused");
}
