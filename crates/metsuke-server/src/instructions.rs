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
    self, AgentId, Envelope, Failure, HEADER_POOL, HEADER_SIGNATURE, HEADER_VKEY, Metric, Payload,
    PayloadLine, PoolId, Provenance, Reason, Scrape, SigningKey,
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

/// Shared by both, so the two documents cannot drift apart visually.
const STYLE: &str = include_str!("../assets/style.css");

/// The Leios wordmark, in each page's header. Inlined rather than served and
/// linked, because the stylesheet is what colours it: the asset is one dark
/// purple, which the dark theme's background very nearly is.
const LOGO: &str = include_str!("../assets/leios-logo.svg");

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

/// A unit for the node, which is not one of ours: the journald setup reads a
/// journal only a systemd unit has, and cardano-node ships no service file to
/// make one. Offered so a pool testing this does not write one first.
pub const NODE_UNIT: &str = include_str!("../../../contrib/cardano-node.service");

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

/// Where the downloadable files are served. The page links these rather than
/// printing them, so an operator runs `curl -O` instead of selecting sixty
/// lines out of a browser.
pub const FILES_PREFIX: &str = "/files/";

/// Every file the page offers, by the name it is served and linked under. On
/// the way out a config is pointed at this deployment, and any file's
/// references to the siblings below become links to them. Nothing else moves.
pub const FILES: [(&str, &str); 8] = [
    ("config.pipe.toml", CONFIG_PIPE),
    ("config.journald.toml", CONFIG_JOURNALD),
    ("config.minimal.toml", CONFIG_MINIMAL),
    ("config.example.toml", CONFIG_EXAMPLE),
    ("metsuke.service", UNIT),
    ("metsuke-journald.service", UNIT_JOURNALD),
    ("node-pipe.conf", PIPE_DROPIN),
    ("cardano-node.service", NODE_UNIT),
];

/// The names the static agent builds are served and linked under. The flake's
/// own package names, so the page and `nix build` agree and
/// `checks.instructions-outputs` can hold them to it.
pub const BINARIES: [&str; 2] = [
    "metsuke-static-x86_64-linux",
    "metsuke-static-aarch64-linux",
];

/// One static agent build this deployment offers, read at startup by the
/// caller: a path that cannot be read is a deployment mistake, and finding it
/// at boot beats finding it when an operator follows the page.
pub struct Binary {
    pub name: &'static str,
    pub bytes: Vec<u8>,
}

/// Both pages and every file they link, ready to serve. `binaries` is empty
/// where the deployment ships none, and the install step then says to build
/// one instead of offering it.
pub fn pages(public_url: &url::Url, binaries: Vec<Binary>) -> Pages {
    let pointed = |config: &str| pointed_at(config, public_url);
    let files = FILES
        .iter()
        .map(|(name, contents)| File {
            name,
            content_type: "text/plain; charset=utf-8",
            bytes: {
                // Only the configs name an upload_url; every shipped file
                // names its siblings.
                let text = match name.ends_with(".toml") {
                    true => pointed(contents),
                    false => contents.to_string(),
                };
                siblings_linked(&text, public_url).into_bytes()
            },
        })
        .chain(binaries.into_iter().map(|binary| File {
            name: binary.name,
            content_type: "application/octet-stream",
            bytes: binary.bytes,
        }))
        .collect::<Vec<File>>();
    Pages {
        quickstart: quickstart(UNIT_JOURNALD, public_url, &files),
        details: details(&pointed(CONFIG_EXAMPLE)),
        files,
    }
}

/// What this module renders, held together so a caller cannot serve one and
/// forget the others.
pub struct Pages {
    pub quickstart: String,
    pub details: String,
    /// Everything served under `FILES_PREFIX`.
    pub files: Vec<File>,
}

/// One file the pages link and the server answers for.
pub struct File {
    pub name: &'static str,
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

/// The install step's commands: downloading the build this deployment offers
/// where it offers one, and building it otherwise. Composed here rather than
/// branched in the template, which has no conditionals and is better for it.
///
/// The nix line is kept either way, because a build from source is the answer
/// for an architecture this server has no binary for.
fn install(offered: &[File], files_url: &str, binary: &str) -> String {
    // One architecture, not both: an operator has one. The other is named in
    // the prose beside this block, which reads the same either way.
    let name = BINARIES[0];
    let lines = match offers_a_build(offered) {
        true => vec![
            "# Download the build for your architecture".to_string(),
            format!("curl -o metsuke {files_url}{name}"),
            String::new(),
            "# Install it where the unit will look for it".to_string(),
            // -D: the directory is standard, but a minimal image can be
            // without it, and the operator meets that as a failed install
            // rather than as a missing path.
            format!("sudo install -D -m 0755 metsuke {binary}"),
        ],
        false => vec![
            "# Build the static agent".to_string(),
            format!("nix build {}#{name}", flake_ref()),
            String::new(),
            "# Install it where the unit will look for it".to_string(),
            format!("sudo install -m 0755 result/bin/metsuke {binary}"),
        ],
    };
    escape(&lines.join("\n"))
}

/// Whether this deployment hands out an agent, which decides how both the
/// try-it and the install step tell an operator to get one.
fn offers_a_build(offered: &[File]) -> bool {
    offered.iter().any(|file| file.name == BINARIES[0])
}

/// How the try-it gets an agent, and what it then runs. Two values rather than
/// one block, because the rest of that snippet is the same either way and reads
/// better in the template than in a string here.
///
/// A downloaded file arrives without its execute bit, so the download path
/// carries the `chmod` and the nix one does not.
fn try_it(offered: &[File], files_url: &str) -> (String, String) {
    let name = BINARIES[0];
    match offers_a_build(offered) {
        // Renamed as it lands, so the try-it runs the same `metsuke` that the
        // install step, the units and every later command name.
        true => (
            escape(&format!(
                "curl -o metsuke {files_url}{name}\nchmod +x metsuke"
            )),
            "./metsuke".to_string(),
        ),
        false => (
            escape(&format!("nix build {}#{name}", flake_ref())),
            "./result/bin/metsuke".to_string(),
        ),
    }
}

/// A shipped config with its upload URL pointed at this deployment, so the only
/// line an operator edits is their pool id. The URL to replace is read out of
/// the file's own `upload_url` rather than matched against a constant here,
/// which would be a second place for the example host to live.
/// A shipped file's references to its siblings, made reachable. In the
/// repository `contrib/config.pipe.toml` is where that file sits; to an
/// operator holding a download it is a path to nothing, and this deployment
/// serves the same file. Driven off `FILES` rather than the `contrib/` prefix,
/// so a name this server does not answer for keeps pointing at the repository
/// instead of becoming a link that 404s.
fn siblings_linked(text: &str, public_url: &url::Url) -> String {
    let files = public_url
        .join(FILES_PREFIX)
        .expect("the files prefix joins onto an absolute URL");
    FILES.iter().fold(text.to_string(), |text, (name, _)| {
        text.replace(&format!("contrib/{name}"), &format!("{files}{name}"))
    })
}

fn pointed_at(config: &str, public_url: &url::Url) -> String {
    let table: toml::Table = config.parse().expect("a shipped config parses as TOML");
    let example = table
        .get("upload_url")
        .and_then(|value| value.as_str())
        .expect("a shipped config sets upload_url");
    let ours = public_url
        .join(crate::http::SUBMIT_PATH)
        .expect("the submission path joins onto an absolute URL");
    config.replace(example, ours.as_str())
}

/// The four steps and nothing else. It links the files rather than printing
/// them, so what it takes is the unit whose paths it tells an operator to put
/// things at, and the URL those links are absolute against.
pub fn quickstart(unit: &str, public_url: &url::Url, offered: &[File]) -> String {
    let files = public_url
        .join(FILES_PREFIX)
        .expect("the files prefix joins onto an absolute URL");
    let binary = exec_start(unit, ExecStartField::Binary);
    let (try_fetch, try_agent) = try_it(offered, files.as_str());
    fill(
        QUICKSTART,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("style", STYLE.trim_end().to_string()),
            ("logo", LOGO.trim_end().to_string()),
            ("DETAILS_PATH", DETAILS_PATH.to_string()),
            ("FILES_PREFIX", FILES_PREFIX.to_string()),
            ("CLIENT_VERSION", CLIENT_VERSION.to_string()),
            // Absolute, because these end up in a `curl` an operator runs
            // somewhere other than the browser that rendered the link.
            ("files_url", escape(files.as_str())),
            ("journal", escape(JOURNAL.trim_end())),
            ("try_fetch", try_fetch),
            ("try_agent", try_agent),
            // The binary path reaches the page inside this block and nowhere
            // else on the quickstart, so it is not a value of its own here.
            ("install", install(offered, files.as_str(), &binary)),
            (
                "config_path",
                escape(&exec_start(unit, ExecStartField::Config)),
            ),
            ("key_path", escape(&credential_source(unit))),
            // Where both of those go, so the step that writes them can make
            // it first. Read off the config path rather than written down,
            // because the unit is what decides it.
            (
                "config_dir",
                escape(&parent_of(&exec_start(unit, ExecStartField::Config))),
            ),
            // Named in the prose beside the install step, as the alternative to
            // downloading one, and nowhere else on this page.
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
            ("style", STYLE.trim_end().to_string()),
            ("logo", LOGO.trim_end().to_string()),
            ("PATH", PATH.to_string()),
            ("HEADER_VKEY", HEADER_VKEY.to_string()),
            ("HEADER_SIGNATURE", HEADER_SIGNATURE.to_string()),
            ("HEADER_POOL", HEADER_POOL.to_string()),
            ("METADATA_LABEL", METADATA_LABEL.to_string()),
            ("metadata", escape(&metadata_json())),
            ("flake", escape(&flake_ref())),
            ("DOCS_PREFIX", docs_prefix()),
            ("REPOSITORY", env!("CARGO_PKG_REPOSITORY").to_string()),
            ("envelope", escape(&example_envelope())),
            ("reasons", failure_reasons()),
            ("backend", escape(&metrics.backend_config())),
            ("traces", escape(&trace_config())),
            ("metrics_url", escape(metrics.url())),
            ("FILES_PREFIX", FILES_PREFIX.to_string()),
            ("binary", escape(&exec_start(UNIT, ExecStartField::Binary))),
            (
                "config_path",
                escape(&exec_start(UNIT, ExecStartField::Config)),
            ),
            // The container example runs the agent itself rather than under a
            // unit, so it names every path the unit would have supplied. Read
            // off the unit for the same reason the quickstart does: written
            // out here they are a second copy that goes stale silently.
            ("key_path", escape(&credential_source(UNIT))),
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

/// The directory a path is in, for the step that has to create it. The root
/// where a path names no directory, which no shipped unit does.
fn parent_of(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|parent| parent.display().to_string())
        .filter(|parent| !parent.is_empty())
        .unwrap_or_else(|| "/".to_string())
}

/// Where a document the details page links is read: the manifest's URL and
/// the default branch, so a link stays right as the file changes.
fn docs_prefix() -> String {
    format!(
        "{}/blob/main/",
        env!("CARGO_PKG_REPOSITORY").trim_end_matches('/')
    )
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
