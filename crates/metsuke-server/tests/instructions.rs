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

/// Something running before any decision, then the decision, then the four
/// steps. The order is the point: an operator who is funnelled into a setup
/// before seeing the others finds out too late.
const QUICKSTART_SECTIONS: [&str; 6] = [
    "Try it",
    "How you'll run it for real",
    "1. Install the agent",
    "2. Configure it",
    "3. Run it",
    "4. Check it",
];

/// What the quickstart leaves out. Each one is a question the steps raise and
/// do not answer, in the order they raise it.
const DETAILS_SECTIONS: [&str; 9] = [
    "Your application code on chain",
    "On NixOS",
    "What leaves your machine",
    "Which key signs",
    "The node's metrics endpoint",
    "What the node has to emit for trace lines",
    "The pipe",
    "The journal",
    "Further reading",
];

/// Whether the documents the details page links are still in the repository
/// is the flake's `instructions-documents` check: the Rust source is filtered
/// to the crates and contrib, so nothing here can see one.
#[test]
fn the_details_page_links_documents_under_the_repository_prefix() {
    let details = instructions::pages(&public_url(), support::test_binaries()).details;
    let repository = env!("CARGO_PKG_REPOSITORY");

    assert!(
        details.contains(&format!("{repository}/blob/main/README.md")),
        "the details page links no document at the repository it came from"
    );
}

#[test]
fn every_outline_section_is_present_and_in_order() {
    let pages = instructions::pages(&public_url(), support::test_binaries());
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
/// to be reachable from it and nothing else has to be. No closing quote in the
/// needle: every link is to a section of it rather than to the top.
/// The configure step writes two files into a directory a fresh host does not
/// have, so it makes it first, and what it makes has to be where those two
/// actually go. Both paths are read off the shipped unit, so a unit that
/// moves them would otherwise leave the step creating the old one.
#[test]
fn the_configure_step_makes_the_directory_its_files_go_in() {
    let quickstart = instructions::pages(&public_url(), support::test_binaries()).quickstart;
    let made = quickstart
        .lines()
        .find_map(|line| line.strip_prefix("sudo mkdir -p "))
        .expect("the configure step makes the directory")
        .to_string();

    for written in quickstart
        .lines()
        // The page is markup, so a path at the end of a command carries the
        // tag that closes the block it is in.
        .filter_map(|line| line.split_whitespace().last())
        .filter_map(|word| word.split('<').next())
        .filter(|word| word.starts_with(&format!("{made}/")))
    {
        assert_eq!(
            std::path::Path::new(written)
                .parent()
                .expect("a path under the directory has one")
                .display()
                .to_string(),
            made,
            "{written:?} is written into a directory the step does not make"
        );
    }
    assert!(
        quickstart.contains(&format!("{made}/")),
        "the step makes {made:?} and writes nothing into it"
    );
}

/// The example node unit is offered because the journald setup needs one, so
/// it has to satisfy what that setup reads: a unit named as the shipped
/// config's `journal_unit`, whose output is still the journal. Either drifting
/// leaves an agent collecting nothing and saying nothing.
#[test]
fn the_example_node_unit_is_the_one_the_journald_setup_reads() {
    let unit = instructions::NODE_UNIT;
    let journal_unit = instructions::CONFIG_JOURNALD
        .lines()
        .find_map(|line| line.strip_prefix("journal_unit = "))
        .expect("the journald config names the node's unit")
        .trim_matches('"')
        .to_string();

    assert!(
        instructions::FILES
            .iter()
            .any(|(name, _)| *name == format!("{journal_unit}.service")),
        "the shipped config follows {journal_unit:?}, which no served unit is named for"
    );
    // Only what the unit sets, not what its header shows an operator running.
    for setting in unit
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| line.strip_prefix("StandardOutput="))
    {
        assert!(
            matches!(setting.trim(), "journal" | "inherit"),
            "the example unit sends its output to {setting:?}, which is not the journal"
        );
    }
}

/// A shipped file names its siblings, and an operator holding a download has no
/// checkout for `contrib/` to resolve against, so what they receive names this
/// server instead. Only served names are rewritten: `contrib/server.example.toml`
/// is the server operator's file and this page does not offer it, so it stays a
/// repository path rather than becoming a link that 404s.
#[test]
fn a_served_file_reaches_the_siblings_it_names() {
    let pages = instructions::pages(&public_url(), support::test_binaries());
    let mut linked = 0;
    for file in pages
        .files
        .iter()
        .filter(|file| file.content_type.starts_with("text/"))
    {
        let text = String::from_utf8(file.bytes.clone()).expect("a shipped text file is UTF-8");
        for (name, _) in instructions::FILES {
            assert!(
                !text.contains(&format!("contrib/{name}")),
                "{} names contrib/{name}, which a download cannot follow",
                file.name
            );
            linked += text
                .matches(&format!("{}{name}", instructions::FILES_PREFIX))
                .count();
        }
    }
    assert!(linked > 0, "no shipped file reaches a sibling at all");
}

/// The agent's startup dump is one line naming every setting, which is what
/// makes it worth pasting and what would make it a nine-hundred-character
/// scrollbar under the page's first example. The page cuts it short by matching
/// how it begins, so a rename in the agent would quietly restore the wall.
#[test]
fn the_page_shows_no_more_of_the_config_dump_than_its_shape() {
    let quickstart = instructions::pages(&public_url(), support::test_binaries()).quickstart;
    // Found by length rather than by prefix, so this test does not agree with
    // the page about the spelling and then pass on both being wrong.
    let dumped = instructions::JOURNAL
        .lines()
        .find(|line| line.len() > 500)
        .expect("the recording carries the agent's config dump");

    assert!(
        !quickstart.contains(dumped),
        "the page shows all {} characters of the config dump",
        dumped.len()
    );
    assert!(
        quickstart.contains(" …}"),
        "the page never abbreviated the config dump, so it is showing something else"
    );
}

#[test]
fn the_quickstart_links_the_details_page() {
    assert!(
        instructions::pages(&public_url(), support::test_binaries())
            .quickstart
            .contains(&format!(r#"href="{}"#, instructions::DETAILS_PATH)),
        "the quickstart never links the details page"
    );
}

/// And every section it links is one that page has, so a rename leaves no link
/// landing at the top of a long document instead of the paragraph it promised.
#[test]
fn every_section_the_quickstart_links_exists() {
    let pages = instructions::pages(&public_url(), support::test_binaries());
    let needle = format!(r#"href="{}#"#, instructions::DETAILS_PATH);
    let mut linked = 0;
    for after in pages.quickstart.split(&needle).skip(1) {
        let anchor = after
            .split('"')
            .next()
            .expect("a split yields a first piece");
        assert!(
            pages.details.contains(&format!(r#"id="{anchor}""#)),
            "the quickstart links #{anchor}, which the details page does not have"
        );
        linked += 1;
    }
    assert!(linked > 0, "the quickstart links no section at all");
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
    let prose = prose(&instructions::pages(&public_url(), support::test_binaries()).details);
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
    let rows = rendered_rows(&instructions::pages(&public_url(), support::test_binaries()).details);
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
    // The block with a blank line in it, not the first block: the page opens
    // on other snippets, and every one of those is a single run of lines.
    let example = snippets(page)
        .into_iter()
        .find(|snippet| snippet.contains("\n\n"))
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
    let files = instructions::pages(&public_url(), support::test_binaries()).files;
    let configs: Vec<&instructions::File> = files
        .iter()
        .filter(|file| file.name.ends_with(".toml"))
        .collect();
    assert!(!configs.is_empty(), "no config is served at all");
    for file in configs {
        let name = file.name;
        let table: toml::Table = String::from_utf8_lossy(&file.bytes)
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
/// configs carry an upload URL to point. The agent builds are served beside
/// them, byte for byte, and are what an operator's `curl` gets.
#[test]
fn every_served_file_is_the_shipped_one() {
    let binaries = support::test_binaries();
    let expected: Vec<(&str, Vec<u8>)> = instructions::FILES
        .iter()
        .map(|(name, shipped)| (*name, shipped.as_bytes().to_vec()))
        .chain(
            binaries
                .iter()
                .map(|binary| (binary.name, binary.bytes.clone())),
        )
        .collect();
    let files = instructions::pages(&public_url(), support::test_binaries()).files;
    assert_eq!(files.len(), expected.len());
    for (name, shipped) in expected {
        let served = files
            .iter()
            .find(|file| file.name == name)
            .unwrap_or_else(|| panic!("{name} is named but not served"));
        assert!(!served.bytes.is_empty(), "{name} is served empty");
        // Only what a deployment alone can resolve is rewritten: a config's
        // upload URL, and the siblings a file names, which are paths into a
        // checkout the operator does not have. Undoing the second recovers the
        // shipped bytes, which is what says nothing else moved. A binary is not
        // text and is compared as it came.
        if !name.ends_with(".toml") {
            let recovered = match String::from_utf8(served.bytes.clone()) {
                Ok(text) => unlinked(&text).into_bytes(),
                Err(_) => served.bytes.clone(),
            };
            assert_eq!(recovered, shipped, "{name} was altered on the way out");
        }
    }
}

/// The reverse of the server's sibling linking, so the compare above can hold
/// every shipped file to being otherwise untouched.
fn unlinked(text: &str) -> String {
    let files = public_url()
        .join(instructions::FILES_PREFIX)
        .expect("the files prefix joins onto an absolute URL");
    instructions::FILES
        .iter()
        .fold(text.to_string(), |text, (name, _)| {
            text.replace(&format!("{files}{name}"), &format!("contrib/{name}"))
        })
}

/// Where a deployment ships builds, every place the page hands an operator an
/// agent offers one. The try-it is the first of those and was for a while the
/// last to know, which is the shape of mistake this catches.
#[test]
fn a_deployment_that_ships_agent_builds_offers_them_everywhere() {
    let quickstart = instructions::pages(&public_url(), support::test_binaries()).quickstart;
    let downloads = quickstart
        .matches(&format!(
            "{}{}",
            instructions::FILES_PREFIX,
            instructions::BINARIES[0]
        ))
        .count();
    assert!(
        downloads >= 2,
        "a page offering builds names one {downloads} times: the try-it and the \
         install step are both places an operator gets the agent"
    );
    // Building is still named, as the alternative and as the answer on an
    // architecture this deployment has no binary for. What must not survive is
    // a command block that hands an operator no download at all.
    assert!(
        quickstart.contains("nix build"),
        "the page never mentions building one instead"
    );
}

/// A deployment that ships no agent build serves none, and its page cannot then
/// offer one: the install step tells an operator to build instead.
#[test]
fn a_deployment_with_no_agent_build_offers_none() {
    let pages = instructions::pages(&public_url(), Vec::new());
    assert_eq!(pages.files.len(), instructions::FILES.len());
    for name in instructions::BINARIES {
        assert!(
            !pages
                .quickstart
                .contains(&format!("curl -O {}", instructions::FILES_PREFIX)),
            "the page offers a download of {name} that nothing serves"
        );
    }
    assert!(
        pages.quickstart.contains("nix build"),
        "the page offers no way to get the agent at all"
    );
}

/// The headers an operator's proxy has to pass through are the same two the
/// agent sends.
#[test]
fn the_page_names_the_submission_headers() {
    let page = instructions::pages(&public_url(), support::test_binaries()).details;
    for header in [HEADER_VKEY, HEADER_SIGNATURE] {
        assert!(page.contains(header), "the page never names {header}");
    }
}

/// Linked, so a browser never falls back to the path it would have guessed
/// (metsuke-n1th). Both pages, because either one can be the first a browser
/// opens.
#[test]
fn the_page_links_the_icon_route() {
    let pages = instructions::pages(&public_url(), support::test_binaries());
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
    let pages = instructions::pages(&public_url(), support::test_binaries());
    // Against the snippets, not the markup: node-pipe.conf names <your-node>,
    // which reaches the page as entities. This is also the stronger claim, that
    // the file is a block to copy rather than text somewhere on the page.
    let carried = |page: &str, shipped: &str| {
        snippets(page)
            .iter()
            .any(|snippet| snippet.contains(shipped.trim_end()))
    };
    // The quickstart carries no config or unit at all now: it links them, so
    // an operator downloads the file rather than selecting it out of a browser.
    // The recording is the exception, because it is matched against their own
    // journal rather than copied anywhere. Line for line rather than whole,
    // because the agent's config dump is cut short there and every other line
    // still has to arrive intact, which is what the matching needs.
    for line in instructions::JOURNAL.trim_end().lines() {
        if line.len() > 500 {
            continue;
        }
        assert!(
            carried(&pages.quickstart, line),
            "the quickstart does not carry the recorded line {line:?} whole"
        );
    }
    // Neither page prints a file it could link. Printing one costs an operator
    // a browser selection instead of a download, and costs every other reader
    // the length of it.
    for shipped in [
        include_str!("../../../contrib/config.example.toml"),
        include_str!("../../../contrib/config.minimal.toml"),
        include_str!("../../../contrib/config.pipe.toml"),
        include_str!("../../../contrib/node-pipe.conf"),
        include_str!("../../../contrib/config.journald.toml"),
        include_str!("../../../contrib/metsuke.service"),
        include_str!("../../../contrib/metsuke-journald.service"),
    ] {
        for page in [&pages.quickstart, &pages.details] {
            assert!(
                !carried(page, shipped),
                "a page prints a file it should be linking"
            );
        }
    }
}

#[test]
fn the_page_nudges_towards_the_agent_version_this_server_was_built_with() {
    assert!(
        instructions::pages(&public_url(), support::test_binaries())
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

/// Every file either page links is one this server answers for. A link to a
/// name nothing serves is a 404 an operator meets while following the steps,
/// and the page is the only place those names are written down.
#[test]
fn every_file_the_pages_link_is_one_the_server_serves() {
    let pages = instructions::pages(&public_url(), support::test_binaries());
    let mut linked = 0;
    for page in [&pages.quickstart, &pages.details] {
        for after in page.split(instructions::FILES_PREFIX).skip(1) {
            // Both the relative hrefs and the absolute curl targets end at the
            // next quote or whitespace.
            let name = after
                .split(['"', '\'', ' ', '<', '\n'])
                .next()
                .expect("a split yields a first piece");
            // Against what this deployment serves, not the compiled-in table:
            // the agent builds are links only where they are offered.
            assert!(
                pages.files.iter().any(|file| file.name == name),
                "a page links {name:?}, which the server does not serve"
            );
            linked += 1;
        }
    }
    assert!(linked > 0, "the pages link no files at all");
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
        .replace("/etc/metsuke/signing-key", "/opt/signing-key");
    let page = instructions::quickstart(&unit, &public_url(), &[]);
    assert!(page.contains("/opt/signing-key"), "key path not followed");
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
    // The quickstart, because it is the page that still takes a shipped file
    // and puts its text on screen: the unit's own paths. The details page links
    // every file it names now, so what it renders is generated rather than
    // read, and there is no shipped text left there to smuggle a tag in.
    //
    // No spaces in the needle: `exec_start` reads the binary out of ExecStart by
    // word, so a spaced one would only ever reach the page in part.
    let page = instructions::quickstart(
        &instructions::UNIT.replace("/usr/local/bin/metsuke", "<b>a&b</b>"),
        &public_url(),
        &[],
    );

    assert!(page.contains("&lt;b&gt;a&amp;b&lt;/b&gt;"));
    assert!(!page.contains("<b>"));
}
