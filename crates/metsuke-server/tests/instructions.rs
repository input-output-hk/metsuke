//! The onboarding page (ticket metsuke-4zo.11), checked for provenance rather
//! than wording: each test moves a value in the file that owns it and asserts
//! the page moved with it (`instructions` says why that is the point).

use metsuke_server::instructions;
use metsuke_wire::envelope::{self, HEADER_POOL_ID};

mod support;
use support::{envelope_for, test_key};

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
/// Derived from the bytes the wire crate seals rather than a list written here,
/// so a v1 field added to `Sample` fails this until the page names it.
#[test]
fn every_v1_field_appears_on_the_page() {
    let page = instructions::page();
    let envelope = envelope_for(&test_key(), 1);
    let header: serde_json::Value =
        serde_json::from_slice(&envelope::header_json(&envelope).unwrap()).unwrap();
    let lines = envelope::payload_lines(&envelope);
    let sample: serde_json::Value =
        serde_json::from_slice(lines.strip_suffix(b"\n").unwrap()).unwrap();
    let fields = header
        .as_object()
        .unwrap()
        .keys()
        .chain(sample.as_object().unwrap().keys());
    for field in fields {
        assert!(
            page.contains(field),
            "the page never names the v1 field {field:?}"
        );
    }
}

/// The headers an operator's proxy has to pass through are the same three the
/// agent sends.
#[test]
fn the_page_names_the_submission_headers() {
    assert!(instructions::page().contains(HEADER_POOL_ID));
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

/// metsuke-4zo.97: the node-config step pins the node's root severity, and
/// pins it to the floor the shipped agent config sets. A node whose root sits
/// above that floor emits nothing for the agent's severity rule to select and
/// says nothing about it.
#[test]
fn the_node_root_severity_comes_from_the_agents_own_floor() {
    // The needle is the example's own min_severity, so a stale one makes the
    // replace a no-op and fails the assert below rather than passing.
    let config = instructions::CONFIG_EXAMPLE
        .replace(r#"min_severity = "Notice""#, r#"min_severity = "Alert""#);
    let page = instructions::render(&config, instructions::UNIT);
    assert!(
        page.contains(r#""severity": "Alert""#),
        "the root severity does not follow the config's min_severity"
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
        let lowered = instructions::LOWERED_NAMESPACES
            .iter()
            .any(|key| key.starts_with(&namespace) || namespace.starts_with(key));
        let at_root = namespace.starts_with(instructions::EMITTED_AT_ROOT_SEVERITY)
            || instructions::EMITTED_AT_ROOT_SEVERITY.starts_with(&namespace);
        assert!(
            lowered || at_root,
            "the node-config step never makes the node emit {namespace:?}"
        );
    }
}

/// The two node-config snippets sit under the same key, so the second has to
/// carry the first whole or an operator applying both loses the backends.
#[test]
fn the_trace_step_carries_the_backend_step_whole() {
    let config = instructions::CONFIG_EXAMPLE.replace("127.0.0.1:12798", "127.0.0.1:19999");
    let page = instructions::render(&config, instructions::UNIT);
    assert_eq!(
        page.matches("PrometheusSimple 127.0.0.1 19999").count(),
        2,
        "the trace snippet does not repeat the backends the metrics snippet set"
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
