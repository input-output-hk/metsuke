//! The onboarding page an operator is pointed at: nothing to accepted
//! submissions. It is rendered rather than checked in so that every value it
//! quotes comes out of the file that owns it. That is the shipped example
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
    let flake = flake_ref();
    format!(
        r#"<!doctype html>
<meta charset="utf-8">
<title>metsuke: telemetry for the MusashiNet rewards program</title>
<style>body {{ max-width: 46em; margin: 2em auto; font-family: sans-serif }}
pre {{ overflow-x: auto; background: #f4f4f4; padding: 1em }}</style>

<h1>metsuke</h1>

<p>metsuke is a small agent you run beside cardano-node. It reads your node's
Prometheus metrics endpoint over loopback and sends this server a signed
submission. It never opens your node socket and never touches a key beyond the one
signing key you point it at. It reads your node's journal only if you turn that
on in step 5, and then only the trace lines your own configuration selects.</p>

<p>Steps 1 to 3 are decisions and on-chain work. Steps 4 to 9 are what you run
on the machine your node is on.</p>

<h2>1. What leaves your machine</h2>

<p>One submission is a plain JSON header, then your scrapes zstd compressed, one
JSON object per line, with a detached Ed25519 signature over the whole byte
sequence. The header rides in a zstd skippable frame, so <code>zstd -d</code>
on a submission hands back the lines and nothing else. This is an example of the
whole thing: two scrapes, the first cut down to two metrics where a real line
carries every one your node exposes, the second a scrape that failed.</p>

<pre>{envelope}</pre>

<p>One line per scrape, and its <code>metrics</code> are every metric your node
exposes on the endpoint you open in step 4 whose value JSON can hold. Those are
the same metric lines that command prints. Each entry carries the metric's
<code>name</code>, its <code>labels</code> and its <code>value</code> as your
node stated them, plus the <code>declared_type</code> its <code># TYPE</code>
line gave where it had one. The agent reads nothing else on your machine. It
contributes two facts of its own, <code>scraped_at</code>, the time it scraped,
and <code>clock_offset_ms</code>, the offset its own NTP query measured.</p>

<p>A scrape that failed is itself a signal, so the submission is sent either way:
no
metrics, and <code>failure</code> naming what stopped it. Its
<code>reason</code> is one of {reasons}, and its <code>detail</code> is the
message the agent had: the port, the status, or the size limit it was
configured with.</p>

<p>Every line names the pool and the machine that wrote it under
<code>metsuke</code>, as <code>pool_id</code> and <code>agent_id</code>, the
name you configure. One line read out of the archive on its own still says where
it came from. That is the only key metsuke claims on a line.</p>

<p>The header carries those same two, and four more: <code>agent_version</code>,
the build that scraped; <code>counter</code>, which submission of yours this is;
<code>timestamp</code>, when the submission was sealed; and
<code>schema_version</code>, which shape its lines are.</p>

<p>If you do step 5, trace lines travel as their own submissions: the same header
with <code>schema_version</code> 2, and then the lines you selected, one per
line, each the object your node wrote plus that same <code>metsuke</code>
key.</p>

<p>The signature travels beside the body in two headers:
<code>{HEADER_VKEY}</code> and <code>{HEADER_SIGNATURE}</code>. Anything between
your agent and this server has to pass both through unchanged, or the signature
will not verify. Your pool id is not among them. It is the hash of the key in
the first header, so this server derives it rather than taking your word for
it.</p>

<h2>2. Register your pool</h2>

<p>Your application to the rewards program carries an application code. Put the
same code in your pool registration transaction's metadata, under label
{METADATA_LABEL}:</p>

<pre>{metadata}</pre>

<p>Only your cold key can sign a pool registration, so the two halves matching
is what shows the application came from you. Until they do, this server refuses
your submissions whatever key you sign them with.</p>

<h2>3. Choose a signing key</h2>

<p>Your pool's cold key signs submissions. That is the whole of it. A pool id is
the hash of its cold verification key, so the key that signs is what says which
pool a submission is for, and nothing else has to be looked up or believed.</p>

<p>The agent reads the key as a cardano-cli TextEnvelope file, the
<code>pool.skey</code> you already have. It refuses to start unless the key
hashes to the <code>pool_id</code> you configured, which is the same check this
server makes on every submission.</p>

<h2>4. Enable the node's metrics endpoint</h2>

<p>cardano-node exposes nothing to scrape until you add the backend. Add it to
your node configuration's <code>TraceOptions</code>, bound to loopback so it is
not reachable from anywhere else:</p>

<pre>{backend}</pre>

<p>If your node configuration has no <code>TraceOptions</code> at all, paste that
as it stands. If it has one, merge into it rather than pasting over it. The
<code>""</code> key is your node's root entry, so keep every other key it has
and add to its backends list.</p>

<p>Both backends have to end up in that list, and each replaces one of its own
kind rather than joining it. If your root already names a
<code>PrometheusSimple</code> or an <code>Stdout</code> backend, replace that one
instead of keeping both. If you would rather keep your own
<code>PrometheusSimple</code>, leave it and point step 7's
<code>metrics_url</code> at its port. Get this wrong and metrics still work,
step 5 looks applied, and not one trace line is ever collected.</p>

<p>Restart the node, then check it answers:</p>

<pre>curl -s {metrics_url}</pre>

<h2>5. Optional: let the node's traces out</h2>

<p>The metrics endpoint is a periodic snapshot. It carries no per-event
timestamps, so it cannot answer when an announcement arrived, when a block body
and its closure were received, or when a quorum was reached. Those live in the
node's trace stream, and the agent ships every field of the lines you select
from it. It reads one field to decide, and it computes nothing from any of
them.</p>

<p>Skip this step and the agent stays exactly as step 4 leaves it: metrics
only, and no read of your journal. To turn it on, the node has to emit those
traces in the first place. These are the namespaces it has to emit, again as
keys to merge into your <code>TraceOptions</code>:</p>

<pre>{traces}</pre>

<p><strong>Check before you add them.</strong> Your configuration may already set
some of these, and merging replaces a key's whole entry, so these take the
place of whatever severity or rate limit you had on those namespaces. It may
also set rate limits on namespaces this snippet does not name; pasting over the
object rather than merging into it would drop those too.</p>

<p>Each namespace carries its own <code>severity</code>, so your node's root
threshold is left as you have it and nothing here depends on where you set it.
There is no <code>""</code> key in this snippet, so it cannot disturb the root
entry step 4 touched. <code>maxFrequency: 0</code> is not a typo, it means no
rate limit, and leaving it out silently caps the stream.</p>

<p>Restart the node. Its lines then go to the journal under its own unit, which
is what the agent's <code>[log]</code> section in step 7 points at. That read
costs the agent membership of the <code>systemd-journal</code> group, the one
privilege it holds beyond scraping loopback; if you would rather it did not
hold that, skip this step and leave <code>[log]</code> out.</p>

<h2>6. Install the agent</h2>

<p>The current agent is {CLIENT_VERSION}. On NixOS, add
<code>{flake}</code> as a flake input and import its
<code>nixosModules.metsuke</code>, which writes the config and the unit for
you; the rest of this page is then a description of what that module does.</p>

<p>Anywhere else, take the static build for your architecture and put it where
the unit expects it:</p>

<pre>nix build {flake}#metsuke-static-x86_64-linux
sudo install -m 0755 result/bin/metsuke {binary}</pre>

<p>Substitute <code>metsuke-static-aarch64-linux</code> on ARM. There is no
install script and no self-update: updating is always something you do
deliberately.</p>

<h2>7. Configure the agent</h2>

<p>Write this to <code>{config_path}</code>; its own comments say which values
you must set.</p>

<pre>{config}</pre>

<p>The upload URL is this server. Replace the example host with the host you
are reading this page on. The metrics URL has to match the endpoint you opened
in step 4, and has to be a loopback address. The agent refuses to scrape
anything else.</p>

<h2>8. Run it under systemd</h2>

<p>This unit runs the agent with no privileges beyond reading its own config
and writing its spool; its own header says where to install it and how to hand
it the signing key. If you did step 5, two directives change: add
<code>SupplementaryGroups=systemd-journal</code>, and turn
<code>ProcSubset=pid</code> into <code>ProcSubset=all</code>. journalctl needs
both, and they are the whole difference; the NixOS module makes them for you
when <code>[log]</code> is set.</p>

<pre>{unit}</pre>

<pre>sudo systemctl daemon-reload
sudo systemctl enable --now metsuke</pre>

<h2>9. Verify</h2>

<p>The agent logs one line at startup naming the endpoint it scrapes and the
pool it reports for, and one line per submission saying whether the server took
it:</p>

<pre>systemctl status metsuke
journalctl -u metsuke -f</pre>

<p>The first submission is sent as soon as the agent starts, so you do not have
to wait out a cadence to find out that something is wrong. A refused submission
logs the server's reason and the scrapes stay spooled. Nothing is lost
while you fix it, and they upload once it is fixed.</p>

<h2>10. Staying up to date</h2>

<p>This server was built against agent {CLIENT_VERSION}, and tells every agent
that uploads to it which version that is. Yours logs a warning when it is
older. To update, repeat step 6 and restart the service:</p>

<pre>sudo systemctl restart metsuke</pre>

<p>The spool is on disk, so queued scrapes survive the restart; nothing is
scraped while the agent is down.</p>
"#,
        envelope = escape(&example_envelope()),
        reasons = failure_reasons(),
        metadata = escape(&metadata_json()),
        backend = escape(&metrics.backend_config()),
        traces = escape(&trace_config()),
        metrics_url = escape(metrics.url()),
        config = escape(config_example.trim_end()),
        unit = escape(unit.trim_end()),
        binary = escape(&binary),
        config_path = escape(&config_path),
        flake = escape(&flake),
    )
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
