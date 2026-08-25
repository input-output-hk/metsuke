//! The onboarding page an operator is pointed at: nothing to accepted
//! submissions. It is rendered rather than checked in so that every value it
//! quotes comes out of the file that owns it — the shipped example config and
//! unit whole, the agent version `build.rs` read, the field list from the wire
//! types — and no edit here can document a default the agent does not ship.

use metsuke_wire::envelope::{
    Envelope, HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, SCHEMA_VERSION, Sample,
    SigningKey,
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
<title>metsuke — telemetry for the MusashiNet rewards program</title>
<style>body {{ max-width: 46em; margin: 2em auto; font-family: sans-serif }}
pre {{ overflow-x: auto; background: #f4f4f4; padding: 1em }}</style>

<h1>metsuke</h1>

<p>metsuke is a small agent you run beside cardano-node. It reads your node's
Prometheus metrics endpoint over loopback and uploads a signed batch to this
server. It never opens your node socket, reads your logs, or touches any key
beyond the one signing key you point it at.</p>

<p>Steps 1 to 3 are decisions and on-chain work. Steps 4 to 8 are what you run
on the machine your node is on.</p>

<h2>1. What leaves your machine</h2>

<p>One upload is one JSON envelope, zstd compressed, with a detached Ed25519
signature over the compressed bytes. This is an example of the whole thing.
Nothing outside these fields is collected:</p>

<pre>{envelope}</pre>

<p>Every sampled field may be <code>null</code>. A scrape that failed is itself
a signal, so the batch uploads either way.</p>

<p>The signature travels beside the body in three headers:
<code>{HEADER_POOL_ID}</code>, <code>{HEADER_VKEY}</code> and
<code>{HEADER_SIGNATURE}</code>. Anything between your agent and this server has
to pass all three through unchanged, or the signature will not verify.</p>

<h2>2. Register your pool</h2>

<p>Your application to the rewards program carries an application code. Put the
same code in your pool registration transaction's metadata, under label
{METADATA_LABEL}:</p>

<pre>{metadata}</pre>

<p>Only your cold key can sign a pool registration, so the two halves matching
is what shows the application came from you. Until they do, this server refuses
your submissions whatever key you sign them with.</p>

<h2>3. Choose a signing key</h2>

<p>Submissions are signed by either your pool's cold key or a Calidus key you
have registered for it. Which one is your policy, not ours — this server checks
both. The cold key works because a pool id is its hash; a Calidus key works
because your cold key witnessed a
<a href="https://cips.cardano.org/cip/CIP-0151">CIP-151</a> registration naming
it, and the registration with the highest nonce is the one that counts.</p>

<p>Either way the agent reads the key as a cardano-cli TextEnvelope file — the
<code>pool.skey</code> or <code>pool.calidus.skey</code> you already have. If
you would rather not put a cold key on the telemetry machine, register a
Calidus key and use that; you can rotate it later by registering another with a
higher nonce.</p>

<h2>4. Enable the node's metrics endpoint</h2>

<p>cardano-node exposes nothing to scrape until you add the backend. In your
node configuration, bound to loopback so it is not reachable from anywhere
else:</p>

<pre>{backend}</pre>

<p>Restart the node, then check it answers:</p>

<pre>curl -s {metrics_url} | head</pre>

<h2>5. Install the agent</h2>

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

<h2>6. Configure the agent</h2>

<p>Write this to <code>{config_path}</code>; its own comments say which values
you must set.</p>

<pre>{config}</pre>

<p>The upload URL is this server: replace the example host with the host you
are reading this page on. The metrics URL has to match the endpoint you opened
in step 4, and has to be a loopback address — the agent refuses to scrape
anything else.</p>

<h2>7. Run it under systemd</h2>

<p>This unit runs the agent with no privileges beyond reading its own config
and writing its spool; its own header says where to install it and how to hand
it the signing key.</p>

<pre>{unit}</pre>

<pre>sudo systemctl daemon-reload
sudo systemctl enable --now metsuke</pre>

<h2>8. Verify</h2>

<p>The agent logs one line at startup naming the endpoint it scrapes and the
pool it reports for, and one line per upload saying whether the server took the
batch:</p>

<pre>systemctl status metsuke
journalctl -u metsuke -f</pre>

<p>The first upload is attempted as soon as the agent starts, so you do not
have to wait out a cadence to find out that something is wrong. A refused
upload logs the server's reason and the samples stay spooled — nothing is lost
while you fix it, and they upload once it is fixed.</p>

<h2>9. Staying up to date</h2>

<p>This server was built against agent {CLIENT_VERSION}, and tells every agent
that uploads to it which version that is. Yours logs a warning when it is
older. To update, repeat step 5 and restart the service:</p>

<pre>sudo systemctl restart metsuke</pre>

<p>The spool is on disk, so queued samples survive the restart; nothing is
sampled while the agent is down.</p>
"#,
        envelope = escape(&example_envelope()),
        metadata = escape(&metadata_json()),
        backend = escape(&metrics.backend_config()),
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

/// The node endpoint both step 4 and step 6 talk about, read once out of the
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

    /// The one node-config change the agent needs, as JSON: cardano-node reads
    /// JSON wherever it reads YAML, and the empty-string key is unwieldy in
    /// YAML by hand.
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

/// One upload, rendered from the wire types themselves: a field this crate can
/// receive but the page does not name is the drift this is here to prevent.
fn example_envelope() -> String {
    let key = SigningKey::from_bytes(&[0u8; 32]);
    let at = OffsetDateTime::from_unix_timestamp(EXAMPLE_INSTANT)
        .expect("a fixed timestamp is in range");
    let envelope = Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: PoolId::from_cold_key(&key.verifying_key()),
        agent_version: CLIENT_VERSION.to_string(),
        counter: 42,
        timestamp: at,
        samples: vec![Sample {
            sampled_at: at,
            block_height: Some(12_318_442),
            slot: Some(163_281_005),
            slot_in_epoch: Some(281_005),
            epoch: Some(587),
            sync_progress: None,
            node_version: Some("11.0.1".to_string()),
            node_revision: Some("0e2b4b1a".to_string()),
            clock_offset_ms: Some(-3),
        }],
    };
    serde_json::to_string_pretty(&envelope).expect("an envelope of plain fields serializes")
}
