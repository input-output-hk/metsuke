//! Which of a node's trace lines the agent ships: a namespace prefix list or a
//! severity floor, both configuration, matched as `or` (ADR 0010 says why not
//! `and`).
//!
//! Only `ns` and `sev` are read, and both as substrings: what is shipped is the
//! node's own bytes, so decoding the rest of a line would be the whole stream's
//! cost paid to reach two fields.

use std::str::FromStr;

use serde::Deserialize;

/// cardano-node's severity ladder, spelled and ordered as the node writes it.
/// Provenance: docs/research/cardano-node-11-tracing.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

#[derive(Debug, thiserror::Error)]
#[error("{found:?} is not one of cardano-node's severities")]
pub struct UnknownSeverity {
    found: String,
}

impl FromStr for Severity {
    type Err = UnknownSeverity;

    fn from_str(name: &str) -> Result<Severity, UnknownSeverity> {
        match name {
            "Debug" => Ok(Severity::Debug),
            "Info" => Ok(Severity::Info),
            "Notice" => Ok(Severity::Notice),
            "Warning" => Ok(Severity::Warning),
            "Error" => Ok(Severity::Error),
            "Critical" => Ok(Severity::Critical),
            "Alert" => Ok(Severity::Alert),
            "Emergency" => Ok(Severity::Emergency),
            found => Err(UnknownSeverity {
                found: found.to_string(),
            }),
        }
    }
}

/// Through `FromStr`, so the config file and a trace line are read by one
/// spelling of the ladder rather than two.
impl<'de> Deserialize<'de> for Severity {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

/// The rules in force, which only `new` builds.
#[derive(Debug)]
pub struct SelectConfig {
    namespaces: Vec<String>,
    min_severity: Severity,
}

/// A namespace rule the host's ceiling does not cover.
#[derive(Debug, thiserror::Error)]
#[error("namespace {namespace:?} is not under any namespace_roots entry ({roots})")]
pub struct OutsideRoots {
    namespace: String,
    roots: String,
}

impl SelectConfig {
    /// The rules, checked against this host's ceiling: every namespace rule has
    /// to sit under a root, including one the server proposes later
    /// (metsuke-4zo.99). The severity floor is not bounded by them. ADR 0010
    /// says why the roots bound the one rule and not the other.
    pub fn new(
        roots: &[String],
        namespaces: Vec<String>,
        min_severity: Severity,
    ) -> Result<SelectConfig, OutsideRoots> {
        for namespace in &namespaces {
            if !roots
                .iter()
                .any(|root| namespace.starts_with(root.as_str()))
            {
                return Err(OutsideRoots {
                    namespace: namespace.clone(),
                    roots: roots.join(", "),
                });
            }
        }
        Ok(SelectConfig {
            namespaces,
            min_severity,
        })
    }
}

/// What one line is. The node prints its whole `NodeConfiguration` before the
/// tracing system is up, so a line carrying neither field is a normal thing to
/// meet once per node start and is skipped like any other unwanted line.
#[derive(Debug, PartialEq)]
pub enum Selection<'a> {
    Ship(&'a str),
    Skip,
}

/// The two keys, as the node writes them: the quote after the colon opens the
/// value. A JSON string holding either would spell it `\"ns\":\"`, so no
/// nested value can be mistaken for a key.
const NS: &str = r#""ns":""#;
const SEV: &str = r#""sev":""#;

/// The namespace a line declares, or `None` when it declares none.
///
/// The *first* `ns`: the node writes `ns` before `data`, and `data` is the only
/// place a nested `ns` can sit. tests/logselect.rs holds that rule against the
/// recording, which is what a node that reorders its keys fails.
pub fn namespace(line: &str) -> Option<&str> {
    string_at(line, line.find(NS)? + NS.len())
}

/// The severity a line declares, or `None` when it declares none or spells one
/// this build does not know.
///
/// The *last* `sev`: `data` precedes it and every key after it is flat, so a
/// nested `sev` can only come before the real one.
pub fn severity(line: &str) -> Option<Severity> {
    string_at(line, line.rfind(SEV)? + SEV.len())?.parse().ok()
}

/// The JSON string open at `start`, up to its closing quote. A value holding
/// an escaped quote comes back short, which costs a rule its match rather than
/// misreading the line — neither a namespace nor a severity name holds one.
fn string_at(line: &str, start: usize) -> Option<&str> {
    let rest = line.get(start..)?;
    rest.find('"').map(|end| &rest[..end])
}

/// Judge one line, against the severity floor first: it is the rule that
/// reaches every namespace, so it settles most of the stream in one compare.
pub fn select<'a>(config: &SelectConfig, line: &'a str) -> Selection<'a> {
    let kept = severity(line).is_some_and(|severity| severity >= config.min_severity)
        || namespace(line).is_some_and(|namespace| {
            config
                .namespaces
                .iter()
                .any(|prefix| namespace.starts_with(prefix.as_str()))
        });
    match kept {
        true => Selection::Ship(line),
        false => Selection::Skip,
    }
}
