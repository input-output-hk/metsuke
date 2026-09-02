//! The onboarding page an operator is pointed at: nothing to accepted
//! submissions. Its values are filled rather than written down, so that every
//! one it quotes comes out of the file that owns it. That is the shipped example
//! config and unit whole, the agent version `build.rs` read, and the field list
//! from the wire types. No edit here can document a default the agent does not
//! ship.

use std::collections::BTreeMap;

use metsuke_wire::envelope::{
    self, AgentId, Envelope, Failure, HEADER_SIGNATURE, HEADER_VKEY, Metric, Payload, PayloadLine,
    PoolId, Provenance, Reason, Scrape, SigningKey,
};
use time::OffsetDateTime;

use crate::CLIENT_VERSION;
use crate::applications::{METADATA_KEY, METADATA_LABEL};

/// Where the page is served. The root, because it is the only thing a person
/// rather than a program comes here for.
pub const PATH: &str = "/";

pub const ICON: &str = include_str!("../assets/favicon.svg");
pub const ICON_PATH: &str = "/favicon.svg";
/// The path a client asks for on its own, whatever the page links. Served
/// because a refusal log is the record of why a pool's uploads are not
/// landing, and an icon probe is not that.
pub const ICON_LEGACY_PATH: &str = "/favicon.ico";
pub const ICON_CONTENT_TYPE: &str = "image/svg+xml";

/// The page's markup, with the placeholders `render` fills. A file rather than
/// a string literal, so editing the most-read document in the project is not
/// editing Rust, and a literal brace is a literal brace.
const TEMPLATE: &str = include_str!("../assets/instructions.html");

/// The shipped agent configuration, whose commented values are pinned to the
/// code's defaults by `crates/metsuke/tests/config.rs`.
pub const CONFIG_EXAMPLE: &str = include_str!("../../../contrib/config.example.toml");

/// The shipped unit, generated from nix/unit.nix and kept current by the
/// flake's `contrib-unit` check.
pub const UNIT: &str = include_str!("../../../contrib/metsuke.service");

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

/// The page, ready to serve.
pub fn page() -> String {
    render(CONFIG_EXAMPLE, UNIT)
}

pub fn render(config_example: &str, unit: &str) -> String {
    let metrics = MetricsEndpoint::from_config(config_example);
    let binary = exec_start(unit, ExecStartField::Binary);
    let config_path = exec_start(unit, ExecStartField::Config);
    fill(
        TEMPLATE,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("HEADER_VKEY", HEADER_VKEY.to_string()),
            ("HEADER_SIGNATURE", HEADER_SIGNATURE.to_string()),
            ("METADATA_LABEL", METADATA_LABEL.to_string()),
            ("CLIENT_VERSION", CLIENT_VERSION.to_string()),
            ("envelope", escape(&example_envelope())),
            ("reasons", failure_reasons()),
            ("metadata", escape(&metadata_json())),
            ("backend", escape(&metrics.backend_config())),
            ("traces", escape(&trace_config())),
            ("metrics_url", escape(metrics.url())),
            ("config", escape(config_example.trim_end())),
            ("unit", escape(unit.trim_end())),
            ("binary", escape(&binary)),
            ("config_path", escape(&config_path)),
            ("flake", escape(&flake_ref())),
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
