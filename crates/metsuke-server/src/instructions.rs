//! The onboarding an operator is pointed at: nothing to accepted submissions.
//! Two documents, because one had to be true for every operator at once and so
//! carried every branch inline. The quickstart is the path a pool takes and
//! stops there; the details page holds what the quickstart leaves out, and is
//! the only one that has to be complete.
//!
//! The analysis page is a third, and its audience is the other direction: a
//! developer reading the archive back rather than a pool sending to it. It is
//! deliberately the shortest of them, because `docs/reading-the-archive.md` is
//! the complete account and stays so. What a page can do that the document
//! cannot is name this deployment, so the walkthrough is the part that has to
//! be run against a particular server and nothing more.
//!
//! Values in each are filled rather than written down, so that every one they
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

/// Where the archive's own onboarding is served. A third document because its
/// audience is not the other two's: a pool operator sends telemetry, and this
/// is for whoever reads it back. Linked from the details page's further
/// reading, which is where a reader who wants the other side already is.
pub const ANALYSIS_PATH: &str = "/analysis";

/// Every page this server serves, in the order the nav lists them, under the
/// heading saying who each group is for.
///
/// Grouped rather than flat, and that is the whole design: the analysis page
/// exists because a developer's tooling in front of a pool operator is a page
/// serving neither. A flat list would put it there on every load. Naming the
/// audience instead makes all three reachable while still saying which are
/// yours, which is what hiding it achieved before at the cost of nobody
/// finding it.
pub const NAV: [(&str, &[(&str, &str)]); 2] = [
    (
        "Running a pool",
        &[(PATH, "Quickstart"), (DETAILS_PATH, "Details")],
    ),
    ("Reading the archive", &[(ANALYSIS_PATH, "Analysis")]),
];

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
const ANALYSIS: &str = include_str!("../assets/analysis.html");

/// Shared by all three, so the documents cannot drift apart visually.
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

/// How the agent's startup dump of every resolved setting begins, which is the
/// line the page shows cut short. A literal because the agent crate is not
/// linked here; `the_page_shows_no_more_of_the_config_dump_than_its_shape`
/// holds the recording to still carrying one.
const CONFIG_LINE: &str = "config: ";

/// The shipped units, generated from nix/unit.nix and kept current by the
/// flake's `contrib-unit` check. `UNIT` is the one the quickstart installs.
pub const UNIT: &str = include_str!("../../../contrib/metsuke.service");
pub const UNIT_JOURNALD: &str = include_str!("../../../contrib/metsuke-journald.service");
pub const PIPE_DROPIN: &str = include_str!("../../../contrib/node-pipe.conf");

/// A unit for the node, which is not one of ours: the journald setup reads a
/// journal only a systemd unit has, and cardano-node ships no service file to
/// make one. Offered so a pool testing this does not write one first.
pub const NODE_UNIT: &str = include_str!("../../../contrib/cardano-node.service");

/// The two duckdb init files the analysis page's read step names. Served for
/// the same reason the configs are: `duckdb -init` takes a path, and a
/// consumer who has downloaded an archive should not have to clone the
/// repository to get the file that reads it.
pub const ANALYTICS_SQL: &str = include_str!("../../../docs/analytics.sql");
pub const ARCHIVE_SQL: &str = include_str!("../../../docs/archive.sql");

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
pub const FILES: [(&str, &str); 10] = [
    ("config.pipe.toml", CONFIG_PIPE),
    ("config.journald.toml", CONFIG_JOURNALD),
    ("config.minimal.toml", CONFIG_MINIMAL),
    ("config.example.toml", CONFIG_EXAMPLE),
    ("metsuke.service", UNIT),
    ("metsuke-journald.service", UNIT_JOURNALD),
    ("node-pipe.conf", PIPE_DROPIN),
    ("cardano-node.service", NODE_UNIT),
    ("analytics.sql", ANALYTICS_SQL),
    ("archive.sql", ARCHIVE_SQL),
];

/// The names the static agent builds are served and linked under. The flake's
/// own package names, so the page and `nix build` agree and
/// `checks.instructions-outputs` can hold them to it.
pub const BINARIES: [&str; 2] = [
    "metsuke-static-x86_64-linux",
    "metsuke-static-aarch64-linux",
];

/// The same for the fetch tool, which the analysis page offers and neither
/// other page mentions. Separate from `BINARIES` rather than appended to it,
/// because that array is what the quickstart's install step indexes and what
/// decides whether this deployment offers an agent at all: a deployment
/// serving only the fetch builds still has no agent to hand a pool.
pub const FETCH_BINARIES: [&str; 2] = [
    "metsuke-fetch-static-x86_64-linux",
    "metsuke-fetch-static-aarch64-linux",
];

/// One static agent build this deployment offers, read at startup by the
/// caller: a path that cannot be read is a deployment mistake, and finding it
/// at boot beats finding it when an operator follows the page.
pub struct Binary {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What the install step tells an operator to check the download against.
/// sha256, and deliberately not the blake2 the rest of this project hashes
/// with: the reader here is an operator, `sha256sum` is what they already have
/// and already trust, and a check that makes them look up a tool is a check
/// they skip.
///
/// What it is worth, and what it is not. It travels inside the page, over the
/// same TLS session as the command beside it, so it says nothing against a
/// server that is itself lying. What it does catch is everything between: a
/// truncated or partially written download, an object half-staged into the
/// store this server reads, and the 404 body `curl` would otherwise leave
/// where the binary was meant to be. Those are the ways this actually goes
/// wrong, and none of them needs an attacker.
fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    metsuke_wire::hex::encode(&Sha256::digest(bytes)[..])
}

/// The digest of the build served under `name`, where this deployment serves
/// one.
fn digest_of(offered: &[File], name: &str) -> Option<String> {
    offered
        .iter()
        .find(|file| file.name == name)
        .map(|file| digest(&file.bytes))
}

/// What a build's checksum file is named, beside the build itself.
pub const CHECKSUM_SUFFIX: &str = ".sha256";

/// The checksum file for one build: the digest and the build's own name, in
/// the format `sha256sum -c` reads, so the check is a command over a file
/// rather than a hex string carried out of a page by eye.
///
/// The name inside it is the build's, so it checks a download that kept its
/// name. Where a snippet renames the build as it lands, the digest goes inline
/// there instead (`try_it`).
///
/// Only the builds have one. Every other file served here is text an operator
/// reads, and a truncated one of those fails to parse in front of them; a
/// truncated build is a file the install step makes executable.
fn checksum_for(build: &File) -> File {
    File {
        name: format!("{}{CHECKSUM_SUFFIX}", build.name),
        content_type: "text/plain; charset=utf-8",
        // Two spaces and a trailing newline, which is what coreutils writes
        // and what `-c` reads back.
        bytes: format!("{}  {}\n", digest(&build.bytes), build.name).into_bytes(),
    }
}

/// Why a served set could not be assembled: two files under one name. Only a
/// deployment's `[downloads]` can name one, because every other entry is this
/// module's own constant.
#[derive(Debug, thiserror::Error)]
#[error("{name} would be served twice: rename or drop the [downloads] entry naming it")]
pub struct Shadowed {
    pub name: String,
}

/// Both pages and every file they link, ready to serve. `binaries` is empty
/// where the deployment ships none, and the install step then says to build
/// one instead of offering it.
pub fn pages(public_url: &url::Url, binaries: Vec<Binary>) -> Result<Pages, Shadowed> {
    let pointed = |config: &str| pointed_at(config, public_url);
    let files = FILES
        .iter()
        .map(|(name, contents)| File {
            name: name.to_string(),
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
        .collect::<Vec<File>>();
    let builds = binaries
        .into_iter()
        .map(|binary| File {
            name: binary.name,
            content_type: "application/octet-stream",
            bytes: binary.bytes,
        })
        .collect::<Vec<File>>();
    // Derived from the builds rather than from the bytes that made them, so a
    // checksum cannot name a build this deployment does not serve.
    let checksums = builds.iter().map(checksum_for).collect::<Vec<File>>();
    // One file per name, and a second one under a name already held is refused
    // rather than resolved. A lookup answers with the first match while the
    // checksums are derived from the builds, so a name held twice publishes the
    // digest of bytes it does not serve, which is worse than publishing none:
    // it teaches an operator that `sha256sum -c` passing means something.
    let mut served: Vec<File> = Vec::new();
    for file in files.into_iter().chain(builds).chain(checksums) {
        if served.iter().any(|held| held.name == file.name) {
            return Err(Shadowed { name: file.name });
        }
        served.push(file);
    }
    let files = served;
    Ok(Pages {
        quickstart: quickstart(UNIT_JOURNALD, public_url, &files),
        details: details(&pointed(CONFIG_EXAMPLE), public_url),
        analysis: analysis(public_url, &files),
        files,
    })
}

/// What this module renders, held together so a caller cannot serve one and
/// forget the others.
pub struct Pages {
    pub quickstart: String,
    pub details: String,
    pub analysis: String,
    /// Everything served under `FILES_PREFIX`.
    pub files: Vec<File>,
}

/// One file the pages link and the server answers for.
pub struct File {
    pub name: String,
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
            "# Download the build for your architecture, and its checksum".to_string(),
            // -f: without it curl writes a 404 body to the destination and
            // exits zero, so a mistyped name becomes an HTML page that the
            // install below makes executable.
            //
            // -O and not -o: the build keeps its own name, which is the name
            // inside the checksum file, so the check below is one command
            // over two files it already has.
            format!("curl -fO {files_url}{name}"),
            format!("curl -fO {files_url}{name}{CHECKSUM_SUFFIX}"),
            String::new(),
            "# Check it is the build this page describes".to_string(),
            format!("sha256sum -c {name}{CHECKSUM_SUFFIX}"),
            String::new(),
            "# Install it where the unit will look for it".to_string(),
            // -D: the directory is standard, but a minimal image can be
            // without it, and the operator meets that as a failed install
            // rather than as a missing path. The rename to `metsuke` happens
            // here, once the bytes have been checked under the name they were
            // checked as.
            format!("sudo install -D -m 0755 {name} {binary}"),
        ],
        false => vec![
            "# Build the static agent".to_string(),
            format!("nix build {}#{name}", flake_ref()),
            String::new(),
            "# Install it where the unit will look for it".to_string(),
            // -D for the reason the branch above gives: it is the same
            // directory, and a minimal image is as likely to be without it
            // whichever way the binary was got.
            format!("sudo install -D -m 0755 result/bin/metsuke {binary}"),
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
                "curl -fo metsuke {files_url}{name}\n\
                 echo '{}  metsuke' | sha256sum -c\n\
                 chmod +x metsuke",
                digest_of(offered, name).unwrap_or_default()
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
///
/// `contrib/` and not also `docs/`, though the two init files are served from
/// there: what those name is themselves, as the path `duckdb -init` loads them
/// from, and `-init` takes a file rather than a URL. Rewriting it would turn a
/// line that runs into one that cannot.
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
    config.replace(example, submit_url(public_url).as_str())
}

/// Where this deployment takes submissions. The one value a shipped file
/// cannot carry, which is why a config is pointed on the way out and why the
/// NixOS example on the details page is filled rather than written down.
fn submit_url(public_url: &url::Url) -> url::Url {
    public_url
        .join(crate::http::SUBMIT_PATH)
        .expect("the submission path joins onto an absolute URL")
}

/// The recording as the page shows it, which is the recording with its config
/// line cut short. That line is every setting the agent resolved, on one line
/// so it can be pasted into a report whole, and it runs to nine hundred
/// characters against sixty for its neighbours. Shown in full it would put a
/// scrollbar under the first example on the page and hide the lines the step
/// is actually about. The fixture itself stays verbatim, because that is what
/// holds it to what the agent prints.
fn journal_shown() -> String {
    /// Enough to recognise the line by when it appears in your own terminal.
    const SHOWN: usize = 52;
    JOURNAL
        .trim_end()
        .lines()
        .map(|line| match line.strip_prefix(CONFIG_LINE) {
            // Every other line is one the step is about, however long.
            None => line.to_string(),
            Some(_) => {
                // Cut at a comma, so it never lands inside a value, and take
                // the index off `char_indices` so it never lands inside a
                // character either.
                let head = line
                    .char_indices()
                    .take_while(|(at, _)| *at <= SHOWN)
                    .filter(|(_, character)| *character == ',')
                    .map(|(at, _)| at)
                    .last()
                    .unwrap_or(0);
                format!("{} …}}", &line[..=head])
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
            ("nav", nav(PATH)),
            ("toc", toc(QUICKSTART)),
            ("sequence", sequence(PATH)),
            ("DETAILS_PATH", DETAILS_PATH.to_string()),
            ("FILES_PREFIX", FILES_PREFIX.to_string()),
            ("CLIENT_VERSION", CLIENT_VERSION.to_string()),
            // Absolute, because these end up in a `curl` an operator runs
            // somewhere other than the browser that rendered the link.
            ("files_url", escape(files.as_str())),
            ("journal", escape(&journal_shown())),
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
pub fn details(config_example: &str, public_url: &url::Url) -> String {
    let metrics = MetricsEndpoint::from_config(config_example);
    fill(
        DETAILS,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("style", STYLE.trim_end().to_string()),
            ("logo", LOGO.trim_end().to_string()),
            ("nav", nav(DETAILS_PATH)),
            ("toc", toc(DETAILS)),
            ("sequence", sequence(DETAILS_PATH)),
            ("PATH", PATH.to_string()),
            ("ANALYSIS_PATH", ANALYSIS_PATH.to_string()),
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
            ("submit_url", escape(submit_url(public_url).as_str())),
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

/// How the analysis page hands over the fetch tool, on the branch the
/// quickstart's install step already takes: a download where this deployment
/// serves one, a build where it does not. Two values for the same reason
/// `try_it` gives, and a downloaded file arrives without its execute bit.
fn fetch_it(offered: &[File], files_url: &str) -> (String, String) {
    let name = FETCH_BINARIES[0];
    match offered.iter().any(|file| file.name == name) {
        true => (
            escape(&format!(
                "curl -fo metsuke-fetch {files_url}{name}\n\
                 echo '{}  metsuke-fetch' | sha256sum -c\n\
                 chmod +x metsuke-fetch",
                digest_of(offered, name).unwrap_or_default()
            )),
            "./metsuke-fetch".to_string(),
        ),
        false => (
            escape(&format!("nix build {}#{name}", flake_ref())),
            "./result/bin/metsuke-fetch".to_string(),
        ),
    }
}

/// The archive's own onboarding, for whoever reads the telemetry back rather
/// than sends it. Takes what the deployment offers for the same reason the
/// quickstart does: the tool is downloaded where this server serves a build
/// and built where it does not.
pub fn analysis(public_url: &url::Url, offered: &[File]) -> String {
    let files = public_url
        .join(FILES_PREFIX)
        .expect("the files prefix joins onto an absolute URL");
    let (fetch_install, fetch) = fetch_it(offered, files.as_str());
    fill(
        ANALYSIS,
        &[
            ("ICON_PATH", ICON_PATH.to_string()),
            ("ICON_CONTENT_TYPE", ICON_CONTENT_TYPE.to_string()),
            ("style", STYLE.trim_end().to_string()),
            ("logo", LOGO.trim_end().to_string()),
            ("nav", nav(ANALYSIS_PATH)),
            ("toc", toc(ANALYSIS)),
            ("sequence", sequence(ANALYSIS_PATH)),
            ("PATH", PATH.to_string()),
            ("DETAILS_PATH", DETAILS_PATH.to_string()),
            ("FILES_PREFIX", FILES_PREFIX.to_string()),
            ("DOCS_PREFIX", docs_prefix()),
            ("REPOSITORY", env!("CARGO_PKG_REPOSITORY").to_string()),
            ("flake", escape(&flake_ref())),
            ("files_url", escape(files.as_str())),
            ("fetch_install", fetch_install),
            // What every later command in the walkthrough runs, so the build
            // branch and the download branch read the same from here on.
            ("fetch", escape(&fetch)),
            // Absolute and this deployment's, because `--server` is the one
            // flag a reader cannot guess and the whole reason this is a served
            // page rather than the markdown it links.
            ("server_url", escape(public_url.as_str())),
            ("REALM", crate::developer::REALM.to_string()),
        ],
    )
}

/// The nav every page carries, with the one being rendered marked. Built from
/// `NAV` rather than written into each template, so a page cannot be missing
/// from one document's copy of it and a renamed path cannot leave two of them
/// pointing at a route this server no longer answers.
fn nav(current: &str) -> String {
    let groups: Vec<String> = NAV
        .iter()
        .map(|(audience, pages)| {
            let items: Vec<String> = pages
                .iter()
                .map(|(path, title)| {
                    // The current page is still a link, so a reader who clicks
                    // it lands where they already are rather than nowhere.
                    // `aria-current` is what says which one it is, and the
                    // stylesheet reads the same attribute.
                    let here = match *path == current {
                        true => " aria-current=\"page\"",
                        false => "",
                    };
                    format!("<li><a href=\"{path}\"{here}>{title}</a>")
                })
                .collect();
            format!(
                "<p class=\"nav-audience\">{audience}</p>\n<ul>\n{}\n</ul>",
                items.join("\n")
            )
        })
        .collect();
    format!(
        "<nav class=\"pages\" aria-label=\"Pages\">\n{}\n</nav>",
        groups.join("\n")
    )
}

/// The pages either side of this one, as the nav orders them. Off `NAV` for
/// the same reason the rail is: a reading order written down twice is one that
/// disagrees with itself the first time a page moves.
///
/// An end of the sequence carries no link rather than a disabled one, so the
/// quickstart does not offer a previous page and the analysis page does not
/// offer a next.
fn sequence(current: &str) -> String {
    let order: Vec<(&str, &str)> = NAV
        .iter()
        .flat_map(|(_, pages)| pages.iter().copied())
        .collect();
    let at = order
        .iter()
        .position(|(path, _)| *path == current)
        .expect("a rendered page is one the nav lists");
    let link = |class: &str, label: &str, entry: Option<&(&str, &str)>| match entry {
        None => String::new(),
        Some((path, title)) => format!(
            "<a class=\"{class}\" href=\"{path}\">\
             <span class=\"sequence-label\">{label}</span>{title}</a>\n"
        ),
    };
    format!(
        "<nav class=\"sequence\" aria-label=\"Pages either side\">\n{}{}</nav>",
        link(
            "prev",
            "Previous",
            at.checked_sub(1).and_then(|before| order.get(before))
        ),
        link("next", "Next", order.get(at + 1)),
    )
}

/// A page's own sections, read out of its `<h2>` headings, so a renamed or
/// reordered section carries its entry with it and a hand-written contents
/// cannot fall behind the document it is for.
///
/// Every heading has to carry an id, which is what the anchor is. Asserted
/// rather than skipped: a section quietly missing from the contents is the
/// failure this would otherwise become, and the pages render once before the
/// listener binds, so it is a startup failure rather than something a reader
/// meets.
fn toc(template: &str) -> String {
    assert_eq!(
        template.matches("<h2").count(),
        template.matches("<h2 id=\"").count(),
        "every section needs an id for the contents to link it by"
    );
    let items: Vec<String> = template
        .split("<h2 id=\"")
        .skip(1)
        .map(|rest| {
            let (id, rest) = rest.split_once("\">").expect("a section's id is quoted");
            let (title, _) = rest.split_once("</h2>").expect("a section heading closes");
            assert!(
                !title.contains('<'),
                "the section {id} carries markup the contents cannot show"
            );
            format!("<li><a href=\"#{id}\">{title}</a>")
        })
        .collect();
    assert!(!items.is_empty(), "a page lists no sections at all");
    format!(
        "<nav class=\"toc\" aria-label=\"On this page\">\n\
         <p class=\"nav-audience\">On this page</p>\n<ul>\n{}\n</ul>\n</nav>",
        items.join("\n")
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
        // Quotes too, so this is safe in an attribute and not only between
        // tags. Nothing needs it today: every attribute a template writes is
        // filled from a compile-time constant, and what a deployment supplies
        // lands in a `pre` or a `code`. But `fill` is blind to where a value
        // is going, so the only thing standing between a config value and an
        // attribute is that nobody has written `href="{{files_url}}"` yet.
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
