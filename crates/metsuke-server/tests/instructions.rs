//! The onboarding pages (ticket metsuke-4zo.11), checked for provenance rather
//! than wording: each test moves a value in the file that owns it and asserts
//! the page moved with it (`instructions` says why that is the point).
//!
//! Which page a test reads is part of what it asserts. The quickstart carries
//! what an operator does; everything about what the agent sends and what the
//! node has to be told is the details page's.

use std::collections::BTreeMap;

use metsuke_server::instructions;
use metsuke_wire::envelope::{
    self, Failure, HEADER_SIGNATURE, HEADER_VKEY, Metric, Reason, Scrape,
};
use time::OffsetDateTime;

mod support;
use support::public_url;

/// The five steps, in the order an operator takes them.
const QUICKSTART_SECTIONS: [&str; 5] = [
    "1. Put your application code on chain",
    "2. Install the agent",
    "3. Write the configuration",
    "4. Install the key and the unit",
    "5. Check it",
];

/// What the quickstart leaves out. Each one is a question the steps raise and
/// do not answer, in the order they raise it.
const DETAILS_SECTIONS: [&str; 6] = [
    "What leaves your machine",
    "Which key signs",
    "The node's metrics endpoint",
    "What the node has to emit for trace lines",
    "The pipe",
    "The journal",
];

#[test]
fn every_outline_section_is_present_and_in_order() {
    let pages = instructions::pages(&public_url());
    for (page, sections) in [
        (&pages.quickstart, &QUICKSTART_SECTIONS[..]),
        (&pages.details, &DETAILS_SECTIONS[..]),
    ] {
        let mut cursor = 0;
        for section in sections {
            let found = page[cursor..]
                .find(section)
                .unwrap_or_else(|| panic!("section {section:?} is missing or out of order"));
            cursor += found + section.len();
        }
    }
}

/// The quickstart is the one an operator is pointed at, so the other page has
/// to be reachable from it and nothing else has to be.
#[test]
fn the_quickstart_links_the_details_page() {
    assert!(
        instructions::pages(&public_url())
            .quickstart
            .contains(&format!(r#"href="{}""#, instructions::DETAILS_PATH)),
        "the quickstart never links the details page"
    );
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
    let prose = prose(&instructions::pages(&public_url()).details);
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
    let rows = rendered_rows(&instructions::pages(&public_url()).details);
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

/// The point of serving them: an operator downloads a config already pointing
/// at this server, so the only line they edit is their pool id.
#[test]
fn every_shipped_config_is_served_pointing_at_this_server() {
    let ours = public_url()
        .join(metsuke_server::http::SUBMIT_PATH)
        .expect("the submission path joins");
    let files = instructions::pages(&public_url()).files;
    let configs: Vec<&(&str, String)> = files
        .iter()
        .filter(|(name, _)| name.ends_with(".toml"))
        .collect();
    assert!(!configs.is_empty(), "no config is served at all");
    for (name, contents) in configs {
        let table: toml::Table = contents
            .parse()
            .unwrap_or_else(|error| panic!("{name} is not TOML after substitution: {error}"));
        assert_eq!(
            table["upload_url"].as_str(),
            Some(ours.as_str()),
            "{name} still points somewhere else"
        );
    }
}

/// Every file the table names has contents, and a unit is left alone: only the
/// configs carry an upload URL to point.
#[test]
fn every_served_file_is_the_shipped_one() {
    let files = instructions::pages(&public_url()).files;
    assert_eq!(files.len(), instructions::FILES.len());
    for (name, shipped) in instructions::FILES {
        let (_, served) = files
            .iter()
            .find(|(served, _)| *served == name)
            .unwrap_or_else(|| panic!("{name} is named but not served"));
        assert!(!served.is_empty(), "{name} is served empty");
        if !name.ends_with(".toml") {
            assert_eq!(served, shipped, "{name} was altered on the way out");
        }
    }
}

/// The headers an operator's proxy has to pass through are the same two the
/// agent sends.
#[test]
fn the_page_names_the_submission_headers() {
    let page = instructions::pages(&public_url()).details;
    for header in [HEADER_VKEY, HEADER_SIGNATURE] {
        assert!(page.contains(header), "the page never names {header}");
    }
}

/// Linked, so a browser never falls back to the path it would have guessed
/// (metsuke-n1th). Both pages, because either one can be the first a browser
/// opens.
#[test]
fn the_page_links_the_icon_route() {
    let pages = instructions::pages(&public_url());
    let link = format!(
        r#"<link rel="icon" href="{}" type="{}">"#,
        instructions::ICON_PATH,
        instructions::ICON_CONTENT_TYPE
    );
    for page in [&pages.quickstart, &pages.details] {
        assert!(page.contains(&link), "no icon link in a page");
    }
}

/// Verbatim, not summarised: the copy-paste block is the shipped file, so
/// there is nothing to keep in step with it.
#[test]
fn the_shipped_config_and_unit_are_carried_whole() {
    let pages = instructions::pages(&public_url());
    // Against the snippets, not the markup: node-pipe.conf names <your-node>,
    // which reaches the page as entities. This is also the stronger claim, that
    // the file is a block to copy rather than text somewhere on the page.
    let carried = |page: &str, shipped: &str| {
        snippets(page)
            .iter()
            .any(|snippet| snippet.contains(shipped.trim_end()))
    };
    // What an operator copies to get running, and nothing they do not need.
    // The recording is not copied but matched, against their own journal.
    for shipped in [
        include_str!("../../../contrib/config.minimal.toml"),
        include_str!("../../../contrib/metsuke.service"),
        include_str!("../../metsuke/tests/fixtures/recordings/agent-journal.log"),
    ] {
        assert!(
            carried(&pages.quickstart, shipped),
            "the quickstart does not carry a shipped file whole"
        );
    }
    // Every other file an operator can end up needing. A pair named on the
    // details page and not carried there is one they have to go and find.
    for shipped in [
        include_str!("../../../contrib/config.example.toml"),
        include_str!("../../../contrib/config.pipe.toml"),
        include_str!("../../../contrib/node-pipe.conf"),
        include_str!("../../../contrib/config.journald.toml"),
        include_str!("../../../contrib/metsuke-journald.service"),
    ] {
        assert!(
            carried(&pages.details, shipped),
            "the details page does not carry a shipped file whole"
        );
    }
}

#[test]
fn the_page_nudges_towards_the_agent_version_this_server_was_built_with() {
    assert!(
        instructions::pages(&public_url())
            .quickstart
            .contains(metsuke_server::CLIENT_VERSION)
    );
}

/// The node-config section tells an operator to open exactly the endpoint the
/// config it is rendered from tells the agent to scrape.
#[test]
fn the_metrics_endpoint_comes_from_the_shipped_config() {
    // The needle is the example's own metrics_url authority; a stale one makes
    // the replace a no-op and fails the asserts below rather than passing.
    let config = instructions::CONFIG_EXAMPLE.replace("127.0.0.1:12798", "127.0.0.1:19999");
    let page = instructions::details(&config);
    assert!(
        page.contains("PrometheusSimple 127.0.0.1 19999"),
        "the backend line does not follow the config's metrics_url"
    );
    assert!(
        page.contains("http://127.0.0.1:19999/metrics"),
        "the check command does not follow the config's metrics_url"
    );
}

/// The same for the quickstart, which is rendered from the minimal config
/// rather than the annotated one.
#[test]
fn the_quickstart_check_command_comes_from_the_config_it_shows() {
    let config = instructions::CONFIG_MINIMAL.replace("127.0.0.1:12798", "127.0.0.1:19999");
    assert!(
        instructions::quickstart(&config, instructions::UNIT)
            .contains("http://127.0.0.1:19999/metrics"),
        "the check command does not follow the config's metrics_url"
    );
}

/// And the two configs agree, so the port the quickstart tells an operator to
/// check is the port the details page tells them to open. Nothing renders both
/// together, so nothing else would catch them drifting.
#[test]
fn every_shipped_config_names_the_same_metrics_endpoint() {
    let needle = "metrics_url = ";
    let of = |config: &str| {
        config
            .lines()
            .find_map(|line| line.trim_start_matches("# ").strip_prefix(needle))
            .expect("a shipped config sets metrics_url")
            .to_string()
    };
    let minimal = of(instructions::CONFIG_MINIMAL);
    for config in [
        instructions::CONFIG_EXAMPLE,
        instructions::CONFIG_PIPE,
        instructions::CONFIG_JOURNALD,
    ] {
        assert_eq!(of(config), minimal, "a shipped config names another port");
    }
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
    let page = instructions::details(instructions::CONFIG_EXAMPLE);
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
    let page = instructions::details(instructions::CONFIG_EXAMPLE);
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
    let page = instructions::details(instructions::CONFIG_EXAMPLE);
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
    let page = instructions::details(instructions::CONFIG_EXAMPLE);
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

/// Where the binary, its config and the signing key go is what the shipped
/// unit says, so the steps that tell an operator to put a file somewhere cannot
/// send it anywhere else. The key is read out of `LoadCredential=` and the
/// other two out of `ExecStart=`.
#[test]
fn the_installed_paths_come_from_the_shipped_unit() {
    // Every needle is one of the shipped unit's own words, and a stale one
    // fails the asserts below rather than passing.
    let unit = instructions::UNIT
        .replace("/usr/local/bin/metsuke", "/opt/bin/metsuke")
        .replace("/etc/metsuke/config.toml", "/opt/metsuke.toml")
        .replace("/etc/metsuke/pool.skey", "/opt/pool.skey");
    let page = instructions::quickstart(instructions::CONFIG_MINIMAL, &unit);
    assert!(page.contains("/opt/pool.skey"), "key path not followed");
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
    for page in [
        instructions::quickstart(
            &instructions::CONFIG_MINIMAL.replace("pool1CHANGEME", "<b>a & b</b>"),
            instructions::UNIT,
        ),
        instructions::details(
            &instructions::CONFIG_EXAMPLE.replace("pool1CHANGEME", "<b>a & b</b>"),
        ),
    ] {
        assert!(page.contains("&lt;b&gt;a &amp; b&lt;/b&gt;"));
        assert!(!page.contains("<b>"));
    }
}
