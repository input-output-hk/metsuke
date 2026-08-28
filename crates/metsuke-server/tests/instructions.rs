//! The onboarding page (ticket metsuke-4zo.11), checked for provenance rather
//! than wording: each test moves a value in the file that owns it and asserts
//! the page moved with it (`instructions` says why that is the point).

use std::collections::BTreeMap;

use metsuke_server::instructions;
use metsuke_wire::envelope::{
    self, Failure, HEADER_SIGNATURE, HEADER_VKEY, Metric, Reason, Scrape,
};
use time::OffsetDateTime;

/// The steps the ticket's outline fixes, in order.
const SECTIONS: [&str; 10] = [
    "1. What leaves your machine",
    "2. Register your pool",
    "3. Choose a signing key",
    "4. Enable the node's metrics endpoint",
    "5. Optional: let the node's traces out",
    "6. Install the agent",
    "7. Configure the agent",
    "8. Run it under systemd",
    "9. Verify",
    "10. Staying up to date",
];

#[test]
fn every_outline_section_is_present_and_in_order() {
    let page = instructions::page();
    let mut cursor = 0;
    for section in SECTIONS {
        let found = page[cursor..]
            .find(section)
            .unwrap_or_else(|| panic!("section {section:?} is missing or out of order"));
        cursor += found + section.len();
    }
}

/// The acceptance criterion: what leaves the box is named field for field.
///
/// The row is built here rather than read off the page, and by a struct literal
/// rather than a list of names: a field added to the wire fails to compile until
/// it is written here, and then fails this until the page's prose names it. A
/// field only a failed scrape carries counts, which is why `failure` is set.
#[test]
fn every_v1_field_appears_on_the_page() {
    // The prose, not the page: the embedded example is rendered from these same
    // types, so a field would appear in it with nothing written about it.
    let prose = prose(&instructions::page());
    let row = serde_json::to_value(Scrape {
        scraped_at: OffsetDateTime::UNIX_EPOCH,
        clock_offset_ms: Some(0),
        failure: Some(Failure {
            reason: Reason::Unreachable,
            detail: String::new(),
        }),
        metrics: vec![Metric {
            name: String::new(),
            labels: BTreeMap::new(),
            value: 0.into(),
            declared_type: None,
        }],
    })
    .unwrap();
    let header: serde_json::Value = serde_json::from_slice(
        &envelope::header_json(&instructions::example_submission()).unwrap(),
    )
    .unwrap();
    let fields = header
        .as_object()
        .unwrap()
        .keys()
        .chain(row.as_object().unwrap().keys())
        .chain(row["failure"].as_object().unwrap().keys())
        .chain(row["metrics"][0].as_object().unwrap().keys());
    for field in fields {
        // As a code span, so a field name is named rather than happening to be
        // a substring of a sentence: `name`, `value` and `detail` are all
        // ordinary words on this page.
        let span = format!("<code>{field}</code>");
        assert!(
            prose.contains(&span),
            "the page's prose never names the v1 field {field:?}"
        );
    }
}

/// The shape a rewards-program developer reads: the agent's own two facts, the
/// failure that is absent when there was none, and the metrics as a list, so
/// one UNNEST reaches a metric and nothing is packed inside a string.
#[test]
fn the_page_renders_rows_whose_metrics_are_a_nested_list() {
    let rows = rendered_rows(&instructions::page());
    for row in &rows {
        let mut keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "clock_offset_ms",
                "failure",
                "metrics",
                "metsuke",
                "scraped_at"
            ]
        );
        assert!(row["metrics"].is_array(), "metrics is not a list: {row}");
    }
    let metrics = rows[0]["metrics"].as_array().unwrap();
    assert!(metrics.len() > 1, "one metric shows no list: {metrics:?}");
    assert!(metrics[0]["name"].is_string());
    assert!(metrics[0]["labels"].is_object());
    assert!(metrics[0]["value"].is_number());
    // The failed row is the one carrying `failure`'s own fields.
    assert!(rows[1]["metrics"].as_array().unwrap().is_empty());
    assert!(rows[1]["failure"]["reason"].is_string());
}

/// The payload lines the page prints, read back out of the page: the example
/// block is the header, a blank line, then one row per line.
fn rendered_rows(page: &str) -> Vec<serde_json::Value> {
    let example = snippets(page)
        .into_iter()
        .next()
        .expect("the page carries the example submission");
    let (_header, rows) = example
        .split_once("\n\n")
        .expect("the example separates its header from its rows");
    rows.lines()
        .map(|line| serde_json::from_str(line).expect("a payload line is JSON"))
        .collect()
}

/// The page with every `<pre>` block dropped: what it says, rather than what it
/// shows.
fn prose(page: &str) -> String {
    let mut prose = String::new();
    let mut rest = page;
    while let Some((before, block)) = rest.split_once("<pre>") {
        prose.push_str(before);
        rest = block
            .split_once("</pre>")
            .expect("every <pre> block is closed")
            .1;
    }
    prose.push_str(rest);
    prose
}

/// The headers an operator's proxy has to pass through are the same two the
/// agent sends.
#[test]
fn the_page_names_the_submission_headers() {
    let page = instructions::page();
    for header in [HEADER_VKEY, HEADER_SIGNATURE] {
        assert!(page.contains(header), "the page never names {header}");
    }
}

/// Verbatim, not summarised: the copy-paste block is the shipped file, so
/// there is nothing to keep in step with it.
#[test]
fn the_shipped_config_and_unit_are_carried_whole() {
    let page = instructions::page();
    for shipped in [
        include_str!("../../../contrib/config.example.toml"),
        include_str!("../../../contrib/metsuke.service"),
    ] {
        assert!(
            page.contains(shipped.trim_end()),
            "the page does not carry a shipped file whole"
        );
    }
}

#[test]
fn the_page_nudges_towards_the_agent_version_this_server_was_built_with() {
    assert!(instructions::page().contains(metsuke_server::CLIENT_VERSION));
}

/// The node-config step tells an operator to open exactly the endpoint the
/// example config then tells the agent to scrape.
#[test]
fn the_metrics_endpoint_comes_from_the_shipped_config() {
    // The needle is the example's own metrics_url authority; a stale one makes
    // the replace a no-op and fails the asserts below rather than passing.
    let config = instructions::CONFIG_EXAMPLE.replace("127.0.0.1:12798", "127.0.0.1:19999");
    let page = instructions::render(&config, instructions::UNIT);
    assert!(
        page.contains("PrometheusSimple 127.0.0.1 19999"),
        "the backend line does not follow the config's metrics_url"
    );
    assert!(
        page.contains("http://127.0.0.1:19999/metrics"),
        "the check command does not follow the config's metrics_url"
    );
}

/// Every `<pre>` block on the page, in order, with the entities `escape` wrote
/// turned back into the characters the snippet is meant to be applied as.
fn snippets(page: &str) -> Vec<String> {
    page.split("<pre>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</pre>"))
        .map(|(block, _)| {
            block
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
        })
        .collect()
}

/// The node-config snippets, as the JSON an operator pastes. Parsed rather than
/// searched, so an assertion about one key cannot read another snippet's.
fn trace_options(page: &str) -> Vec<serde_json::Value> {
    snippets(page)
        .iter()
        .filter_map(|snippet| serde_json::from_str::<serde_json::Value>(snippet).ok())
        .filter(|value| value.get("TraceOptions").is_some())
        .collect()
}

/// Neither node-config snippet sets a root severity. Why the page states no
/// floor an operator has to stay under: ADR 0010.
#[test]
fn no_node_config_snippet_sets_a_root_severity() {
    let page = instructions::render(instructions::CONFIG_EXAMPLE, instructions::UNIT);
    let snippets = trace_options(&page);
    assert_eq!(snippets.len(), 2, "the page lost a TraceOptions snippet");
    // Step 4's is the one with a root entry, and `backends` is what says it is
    // still there: a dropped root indexes to Null, whose `get` answers None for
    // every key, so the severity assert alone would pass over nothing at all.
    let root = &snippets[0]["TraceOptions"][""];
    assert!(
        root.get("backends").is_some(),
        "the backend snippet lost its root entry: {root}"
    );
    assert!(
        root.get("severity").is_none(),
        "the root TraceOptions entry sets a severity: {root}"
    );
    // Step 5's has no root at all (`the_trace_step_cannot_disturb_the_root_entry`),
    // so there is no second root to check.
    assert!(
        snippets[1]["TraceOptions"][""].is_null(),
        "the trace snippet grew a root entry"
    );
}

/// Why each namespace is named rather than inheriting: ADR 0010.
#[test]
fn every_named_namespace_carries_its_own_severity() {
    let page = instructions::render(instructions::CONFIG_EXAMPLE, instructions::UNIT);
    let snippets = trace_options(&page);
    assert_eq!(snippets.len(), 2, "the page lost a TraceOptions snippet");
    // The second: step 5's, the namespace keys.
    let traces = &snippets[1];
    assert!(
        !instructions::NAMED_NAMESPACES.is_empty(),
        "an empty list would assert nothing below"
    );
    for namespace in instructions::NAMED_NAMESPACES {
        assert_eq!(
            traces["TraceOptions"][namespace]["severity"], "Info",
            "{namespace} inherits the root severity"
        );
    }
}

/// The agent parses each trace line as a JSON object, which is what `Stdout
/// MachineFormat` writes and no other `Stdout` backend does. Only the snippet is
/// checked here: what step 4 says about *applying* it rests on cardano-node's
/// backend resolution order, which nothing in this repo verifies. See the note
/// on `instructions::MetricsEndpoint::backend_config` (metsuke-jfb.24).
#[test]
fn the_backend_step_names_the_machine_format_backend() {
    let page = instructions::render(instructions::CONFIG_EXAMPLE, instructions::UNIT);
    let backends = trace_options(&page)[0]["TraceOptions"][""]["backends"]
        .as_array()
        .expect("the root entry lists backends")
        .iter()
        .filter_map(|value| value.as_str().map(str::to_string))
        .collect::<Vec<String>>();
    assert!(
        backends
            .iter()
            .any(|backend| backend == "Stdout MachineFormat"),
        "the snippet does not name the backend the agent can parse: {backends:?}"
    );
}

/// The namespaces the shipped agent config selects, read out of the file that
/// ships them: the example comments them out, and the value is a TOML array
/// either way.
fn shipped_namespaces(config_example: &str) -> Vec<String> {
    let array = config_example
        .lines()
        .find_map(|line| line.trim_start_matches("# ").strip_prefix("namespaces = "))
        .expect("the shipped example config documents namespaces");
    let table: toml::Table = format!("namespaces = {array}")
        .parse()
        .expect("the documented namespaces are a TOML array");
    table["namespaces"]
        .as_array()
        .expect("namespaces is an array")
        .iter()
        .map(|value| value.as_str().expect("a namespace is a string").to_string())
        .collect()
}

/// Every namespace the agent is shipped selecting is one the node-config step
/// makes the node emit. They agree by hand otherwise: an operator who follows
/// both files and gets no lines has nothing telling them which of the two
/// moved (metsuke-4zo.100 review).
#[test]
fn the_node_config_step_covers_every_namespace_the_agent_selects() {
    let namespaces = shipped_namespaces(instructions::CONFIG_EXAMPLE);
    assert!(!namespaces.is_empty(), "the needle found no namespaces");
    for namespace in namespaces {
        // Either direction: the agent selects by prefix, so its rule may sit
        // above the node's namespace or below it.
        let named = instructions::NAMED_NAMESPACES
            .iter()
            .any(|key| key.starts_with(&namespace) || namespace.starts_with(key));
        assert!(
            named,
            "the node-config step never makes the node emit {namespace:?}"
        );
    }
}

/// Both snippets are keys to merge into an operator's own `TraceOptions`, and
/// the trace one holds no `""` key at all, so applying it cannot drop the root
/// entry, which is what carries their severity, their detail and the backends
/// step 4 added. Why merging is the instruction: ADR 0010.
#[test]
fn the_trace_step_cannot_disturb_the_root_entry() {
    let page = instructions::render(instructions::CONFIG_EXAMPLE, instructions::UNIT);
    let snippets = trace_options(&page);
    assert_eq!(snippets.len(), 2, "the page lost a TraceOptions snippet");
    let traces = snippets[1]["TraceOptions"]
        .as_object()
        .expect("TraceOptions is an object");
    assert!(
        !traces.contains_key(""),
        "the trace snippet carries a root entry: {traces:?}"
    );
    // A literal, not NAMED_NAMESPACES.len(): comparing the snippet against the
    // const it was built from is 0 == 0 on an empty const, and "touches no
    // root" would then be true of nothing.
    assert_eq!(
        traces.len(),
        4,
        "the trace snippet names a different number of namespaces: {traces:?}"
    );
}

/// Where the binary and its config go is what the shipped unit's ExecStart
/// says, so the install and configure steps cannot send them elsewhere.
#[test]
fn the_installed_paths_come_from_the_shipped_unit() {
    // Both needles are the shipped unit's own ExecStart words, and a stale one
    // fails the asserts below rather than passing.
    let unit = instructions::UNIT
        .replace("/usr/local/bin/metsuke", "/opt/bin/metsuke")
        .replace("/etc/metsuke/config.toml", "/opt/metsuke.toml");
    let page = instructions::render(instructions::CONFIG_EXAMPLE, &unit);
    assert!(
        page.contains("/opt/metsuke.toml"),
        "config path not followed"
    );
    assert!(
        page.contains("/opt/bin/metsuke"),
        "binary path not followed"
    );
    assert!(
        !page.contains("/usr/local/bin/metsuke"),
        "the page still names a path the unit does not"
    );
}

/// A `<` reaching the browser unescaped would end the block it is inside and
/// swallow the rest of a step.
#[test]
fn shipped_text_is_escaped_into_the_page() {
    let page = instructions::render(
        &instructions::CONFIG_EXAMPLE.replace("pool1CHANGEME", "<b>a & b</b>"),
        instructions::UNIT,
    );
    assert!(page.contains("&lt;b&gt;a &amp; b&lt;/b&gt;"));
    assert!(!page.contains("<b>"));
}
