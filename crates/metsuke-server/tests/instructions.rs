//! The onboarding page (ticket metsuke-4zo.11), checked for provenance rather
//! than wording: each test moves a value in the file that owns it and asserts
//! the page moved with it (`instructions` says why that is the point).

use metsuke_server::instructions;
use metsuke_wire::envelope::HEADER_POOL_ID;

mod support;
use support::{envelope_for, test_key};

/// The steps the ticket's outline fixes, in order.
const SECTIONS: [&str; 9] = [
    "1. What leaves your machine",
    "2. Register your pool",
    "3. Choose a signing key",
    "4. Enable the node's metrics endpoint",
    "5. Install the agent",
    "6. Configure the agent",
    "7. Run it under systemd",
    "8. Verify",
    "9. Staying up to date",
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
/// Derived from the wire types rather than a list written here, so a v1 field
/// added to `Sample` fails this until the page names it.
#[test]
fn every_v1_field_appears_on_the_page() {
    let page = instructions::page();
    let envelope = serde_json::to_value(envelope_for(&test_key(), 1)).unwrap();
    let sample = envelope["samples"][0].clone();
    let fields = envelope
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
