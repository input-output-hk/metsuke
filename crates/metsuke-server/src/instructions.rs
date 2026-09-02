//! The onboarding an operator is pointed at: nothing to accepted submissions.
//! Two documents, because one had to be true for every operator at once and so
//! carried every branch inline. The quickstart is the path a pool takes and
//! stops there; the details page holds what the quickstart leaves out, and is
//! the only one that has to be complete.
//!
//! Values in both are filled rather than written down, so that every one they
//! quote comes out of the file that owns it. That is the shipped configs and
//! units whole, the agent version `build.rs` read, and the field list from the
//! wire types. No edit here can document a default the agent does not ship.

use std::collections::BTreeMap;

use metsuke_wire::envelope::{
    self, AgentId, Envelope, Failure, HEADER_SIGNATURE, HEADER_VKEY, Metric, Payload, PayloadLine,
    PoolId, Provenance, Reason, Scrape, SigningKey,
};
use time::OffsetDateTime;

use crate::CLIENT_VERSION;
use crate::applications::{METADATA_KEY, METADATA_LABEL};

/// Where the quickstart is served. The root, because it is the only thing a
/// person rather than a program comes here for.
pub const PATH: &str = "/";

/// Where the rest of it is served, linked from the quickstart and from nowhere
/// else.
pub const DETAILS_PATH: &str = "/details";

pub const ICON: &str = include_str!("../assets/favicon.svg");
pub const ICON_PATH: &str = "/favicon.svg";
/// The path a client asks for on its own, whatever the page links. Served
/// because a refusal log is the record of why a pool's uploads are not
/// landing, and an icon probe is not that.
pub const ICON_LEGACY_PATH: &str = "/favicon.ico";
pub const ICON_CONTENT_TYPE: &str = "image/svg+xml";

/// Each page's markup, with the placeholders its render fills. Files rather
/// than string literals, so editing the most-read documents in the project is
/// not editing Rust, and a literal brace is a literal brace.
const QUICKSTART: &str = include_str!("../assets/quickstart.html");
const DETAILS: &str = include_str!("../assets/details.html");

/// The shipped agent configurations, one per log source. Their required values
/// are tied to the code's defaults by `crates/metsuke/tests/config.rs`, which
/// is also what pins the example's commented ones.
pub const CONFIG_MINIMAL: &str = include_str!("../../../contrib/config.minimal.toml");
pub const CONFIG_PIPE: &str = include_str!("../../../contrib/config.pipe.toml");
pub const CONFIG_JOURNALD: &str = include_str!("../../../contrib/config.journald.toml");
pub const CONFIG_EXAMPLE: &str = include_str!("../../../contrib/config.example.toml");

/// What a working agent prints, recorded off a real run of the built binary by
/// `the_journal_lines_the_page_shows_are_the_ones_the_agent_prints`, which also
/// fails when the agent stops printing it. From the agent's own fixtures,
/// because that run is the only place these lines exist.
pub const JOURNAL: &str = include_str!("../../metsuke/tests/fixtures/recordings/agent-journal.log");

/// The shipped units, generated from nix/unit.nix and kept current by the
/// flake's `contrib-unit` check. `UNIT` is the one the quickstart installs.
pub const UNIT: &str = include_str!("../../../contrib/metsuke.service");
pub const UNIT_JOURNALD: &str = include_str!("../../../contrib/metsuke-journald.service");
pub const PIPE_DROPIN: &str = include_str!("../../../contrib/node-pipe.conf");

/// The node namespaces the trace step gives an explicit severity. These are the
/// node's own namespaces, not the agent's selection prefixes: what a node emits
/// and what the agent keeps are two settings in two files. Why each entry, and
/// why each gets one: docs/research/cardano-node-11-tracing.md.
pub const NAMED_NAMESPACES: [&str; 4] = [
    "Consensus.LeiosKernel",
    "Consensus.LeiosPeer",
    "Forge.Loop.AdoptedBlock",
    "ChainDB.AddBlockEvent.AddedToCurrentChain",
];

/// Both pages, ready to serve.
pub fn pages() -> Pages {
    Pages {
        quickstart: quickstart(CONFIG_MINIMAL, UNIT),
        details: details(CONFIG_EXAMPLE),
    }
}

/// What this module renders, held together so a caller cannot serve one and
/// forget the other.
pub struct Pages {
    pub quickstart: String,
    pub details: String,
}

/// The five steps and nothing else. Takes the config it shows and the unit it
/// installs, because the paths it quotes are read back out of them.
pub fn quickstart(config: &str, unit: &str) -> String {
    let metrics = MetricsEndpoint::from_config(config);
    fill(
        QUICKSTART,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("DETAILS_PATH", DETAILS_PATH.to_string()),
            ("METADATA_LABEL", METADATA_LABEL.to_string()),
            ("CLIENT_VERSION", CLIENT_VERSION.to_string()),
            ("submit_path", crate::http::SUBMIT_PATH.to_string()),
            ("metadata", escape(&metadata_json())),
            ("metrics_url", escape(metrics.url())),
            ("config", escape(config.trim_end())),
            ("unit", escape(unit.trim_end())),
            ("journal", escape(JOURNAL.trim_end())),
            ("binary", escape(&exec_start(unit, ExecStartField::Binary))),
            (
                "config_path",
                escape(&exec_start(unit, ExecStartField::Config)),
            ),
            ("key_path", escape(&credential_source(unit))),
            ("flake", escape(&flake_ref())),
        ],
    )
}

/// Everything the quickstart leaves out. Takes the annotated example, which is
/// the one config it shows whole and the one the metrics endpoint is read from.
pub fn details(config_example: &str) -> String {
    let metrics = MetricsEndpoint::from_config(config_example);
    fill(
        DETAILS,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("PATH", PATH.to_string()),
            ("HEADER_VKEY", HEADER_VKEY.to_string()),
            ("HEADER_SIGNATURE", HEADER_SIGNATURE.to_string()),
            ("envelope", escape(&example_envelope())),
            ("reasons", failure_reasons()),
            ("backend", escape(&metrics.backend_config())),
            ("traces", escape(&trace_config())),
            ("metrics_url", escape(metrics.url())),
            ("config_example", escape(config_example.trim_end())),
            ("pipe_config", escape(CONFIG_PIPE.trim_end())),
            ("pipe_dropin", escape(PIPE_DROPIN.trim_end())),
            ("journald_config", escape(CONFIG_JOURNALD.trim_end())),
            ("journald_unit", escape(UNIT_JOURNALD.trim_end())),
            ("binary", escape(&exec_start(UNIT, ExecStartField::Binary))),
            (
                "config_path",
                escape(&exec_start(UNIT, ExecStartField::Config)),
            ),
        ],
    )
}

/// Substitute the template's `{{name}}` placeholders. A name nothing fills, and
/// a value the template never names, both panic: the page renders once before
/// the listener binds, so either is a startup failure rather than something an
/// operator can reach.
///
/// One pass, and a filled value is never re-scanned, so the `}}` a compact JSON
/// example ends on cannot read as a placeholder.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut page = String::with_capacity(template.len());
    let mut filled = vec![false; values.len()];
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        page.push_str(&rest[..start]);
        let (name, tail) = rest[start + "{{".len()..]
            .split_once("}}")
            .expect("every placeholder the template opens is closed");
        let at = values
            .iter()
            .position(|(key, _)| *key == name)
            .unwrap_or_else(|| panic!("the template names {name}, which nothing fills"));
        page.push_str(&values[at].1);
        filled[at] = true;
        rest = tail;
    }
    page.push_str(rest);
    for ((name, _), filled) in values.iter().zip(filled) {
        assert!(filled, "the template does not name {name}");
    }
    page
}

/// The characters that would otherwise start a tag or an entity.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The node endpoint both step 4 and step 7 talk about, read once out of the
/// example config so the two cannot name different ports.
struct MetricsEndpoint {
    url: String,
    host: String,
    port: u16,
}

impl MetricsEndpoint {
    fn from_config(config_example: &str) -> MetricsEndpoint {
        let table: toml::Table = config_example
            .parse()
            .expect("the shipped example config parses as TOML");
        let url = table
            .get("metrics_url")
            .and_then(|value| value.as_str())
            .expect("the shipped example config sets metrics_url");
        let parsed = url::Url::parse(url).expect("the example metrics_url parses as a URL");
        MetricsEndpoint {
            host: parsed
                .host_str()
                .expect("the example metrics_url has a host")
                .to_string(),
            // Not `port_or_known_default`: a scheme default would render the
            // node-config line as fact for a port the example never stated.
            port: parsed.port().expect("the example metrics_url has a port"),
            url: url.to_string(),
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    /// The one node-config change the agent needs, as JSON. cardano-node reads
    /// JSON wherever it reads YAML, and the empty-string key is unwieldy in
    /// YAML by hand.
    ///
    /// Why step 4 says to replace a backend of the same kind rather than add
    /// one: cardano-node resolves the root's `PrometheusSimple` with
    /// `listToMaybe` (`Cardano/Node/Tracing/API.hs`, read at the
    /// `cardano-node-leios` pin), so a second is silently ignored and an
    /// operator who appends keeps the port they had. Stated here rather than on
    /// the page, because an operator needs the instruction, not the mechanism,
    /// and nothing in this repo verifies another project's resolution order.
    fn backend_config(&self) -> String {
        format!(
            r#"{{
  "TraceOptions": {{
    "": {{ "backends": ["Stdout MachineFormat", "PrometheusSimple {host} {port}"] }}
  }}
}}"#,
            host = self.host,
            port = self.port,
        )
    }
}

/// What a node has to be told before the trace namespaces the rewards program
/// asked about reach a backend at all. Namespace keys only: it holds no `""`
/// entry, so merging it cannot disturb the root the backend snippet touched, and
/// an operator who already has these namespaces configured keeps whatever else
/// they set on them. Why it sets no root `severity`: ADR 0010.
///
/// Free of `MetricsEndpoint`, unlike `backend_config`: the host and port went
/// with the root entry this no longer writes.
fn trace_config() -> String {
    let named = NAMED_NAMESPACES
        .iter()
        .map(|namespace| {
            format!(
                r#"
    "{namespace}": {{ "severity": "Info", "maxFrequency": 0 }},"#
            )
        })
        .collect::<String>();
    // Each entry brings its own trailing comma, and the last one is not valid
    // JSON. `trim_end_matches` rather than `strip_suffix` because an empty list
    // leaves no comma to strip.
    format!(
        r#"{{
  "TraceOptions": {{{}
  }}
}}"#,
        named.trim_end_matches(',')
    )
}

/// Which path out of the shipped unit's `ExecStart` a step needs.
enum ExecStartField {
    Binary,
    Config,
}

/// Where the unit says the binary and its config live. Read out of the unit
/// rather than repeated, so the install and configure steps put things exactly
/// where the unit will look for them.
fn exec_start(unit: &str, field: ExecStartField) -> String {
    let command = unit
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .expect("the shipped unit has an ExecStart");
    let mut words = command.split_whitespace();
    let found = match field {
        ExecStartField::Binary => words.next(),
        ExecStartField::Config => words.by_ref().skip_while(|word| *word != "--config").nth(1),
    };
    found
        .expect("the shipped unit's ExecStart names the binary and its config")
        .to_string()
}

/// Where the unit expects the signing key. Read out of `LoadCredential=` for
/// the same reason the two paths above are read out of `ExecStart=`: the
/// quickstart tells an operator to put a file somewhere, and the somewhere has
/// to be where the unit will look.
fn credential_source(unit: &str) -> String {
    unit.lines()
        .find_map(|line| line.strip_prefix("LoadCredential="))
        .and_then(|value| value.split_once(':'))
        .expect("the shipped unit loads the signing key as a credential")
        .1
        .to_string()
}

/// How the repository is named to `nix build`. The manifest holds the browser
/// URL, which is the same two path segments.
fn flake_ref() -> String {
    let repository = env!("CARGO_PKG_REPOSITORY");
    let path = repository
        .strip_prefix("https://github.com/")
        .expect("the manifest's repository is a GitHub URL");
    format!("github:{path}")
}

/// Both halves of the gate, as the metadata file half looks on chain.
fn metadata_json() -> String {
    format!(r#"{{"{METADATA_LABEL}": {{"{METADATA_KEY}": "YOUR-CODE"}}}}"#)
}

/// The instant the example is stamped with. Fixed, so the page is the same in
/// every build; the digits mean nothing beyond showing the format.
const EXAMPLE_INSTANT: i64 = 1_780_000_000;

/// The submission the page shows, built from the wire types themselves, so the
/// example cannot show a shape the crate does not send.
/// `the_page_renders_rows_whose_metrics_are_a_nested_list` reads the rows back
/// out of the rendered page rather than restating them.
///
/// Two rows, because a scrape has two shapes and a field only the failed one
/// carries would otherwise never reach the page. Two metrics in the first,
/// where a real row carries every one the endpoint returned. The names are a
/// node's, the values are not, and an operator checks the claim against their
/// own endpoint with the command in step 4.
pub fn example_submission() -> Envelope {
    let key = SigningKey::from_bytes(&[0u8; 32]);
    let at = OffsetDateTime::from_unix_timestamp(EXAMPLE_INSTANT)
        .expect("a fixed timestamp is in range");
    let provenance = Provenance {
        pool_id: PoolId::from_cold_key(&key.verifying_key()),
        agent_id: AgentId::slugify("relay-1").expect("a fixed name slugifies"),
    };
    let rows = [
        Scrape {
            scraped_at: at,
            clock_offset_ms: Some(-3),
            failure: None,
            metrics: vec![
                Metric {
                    name: "cardano_node_metrics_blockNum_int".to_string(),
                    labels: BTreeMap::new(),
                    value: 12_318_442.into(),
                    declared_type: Some("gauge".to_string()),
                },
                Metric {
                    name: "cardano_node_metrics_tipBlock".to_string(),
                    labels: BTreeMap::from([("hash".to_string(), "0e2b4b1a".repeat(8))]),
                    value: 1.into(),
                    declared_type: Some("info".to_string()),
                },
            ],
        },
        Scrape {
            scraped_at: at + time::Duration::minutes(5),
            clock_offset_ms: None,
            failure: Some(Failure {
                reason: Reason::Unreachable,
                detail: "the endpoint did not answer: connection refused".to_string(),
            }),
            metrics: Vec::new(),
        },
    ];
    Envelope::new(
        provenance.clone(),
        CLIENT_VERSION.to_string(),
        42,
        at,
        Payload::scrapes(
            rows.iter()
                .map(|row| PayloadLine::scrape(row, &provenance).expect("plain fields stamp"))
                .collect(),
        ),
    )
}

/// Every reason a failed scrape can give, as code spans, in the order
/// `Reason::ALL` lists them. Rendered from that list, which the wire crate's
/// own const assertion keeps complete, so a case the wire gains is a case the
/// page names.
fn failure_reasons() -> String {
    let words: Vec<String> = Reason::ALL
        .iter()
        .map(|reason| {
            let word = serde_json::to_value(reason).expect("a unit variant serializes");
            let word = word.as_str().expect("as a string");
            format!("<code>{}</code>", escape(word))
        })
        .collect();
    words.join(", ")
}

/// That submission as the page prints it: the header indented for reading,
/// though on the wire it is one line, and the payload after it as the lines a
/// decompressor hands back.
fn example_envelope() -> String {
    let envelope = example_submission();
    let header: serde_json::Value =
        serde_json::from_slice(&envelope::header_json(&envelope).expect("plain fields serialize"))
            .expect("a header is a JSON object");
    let header = serde_json::to_string_pretty(&header).expect("a parsed header re-renders");
    let lines =
        String::from_utf8(envelope::payload_lines(&envelope)).expect("serde_json writes UTF-8");
    format!("{header}\n\n{lines}")
}
