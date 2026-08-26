//! Which of a node's trace lines the agent ships: a namespace prefix list or a
//! severity floor, both configuration, matched as `or` (ADR 0010 says why not
//! `and`).
//!
//! A line is parsed as a JSON object and only `ns` and `sev` are read off it.
//! Parsing decides; it does not produce what goes on the wire.

use std::borrow::Cow;
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

/// A value read out of the line, copied only when the line does not spell it
/// literally. It carries the borrow on its own type because `#[serde(borrow)]`
/// does not reach through an `Option`: on `Option<Cow<'a, str>>` the attribute
/// is accepted and every value still arrives owned.
#[derive(Deserialize)]
#[serde(transparent)]
struct Read<'a>(#[serde(borrow)] Cow<'a, str>);

/// The keys as the node spells them, read off the object's own top level so a
/// `data` payload carrying either is not a candidate.
#[derive(Deserialize)]
struct Keys<'a> {
    #[serde(borrow)]
    ns: Option<Read<'a>>,
    #[serde(borrow)]
    sev: Option<Read<'a>>,
}

/// What one line declares, of the two fields a rule reads. Every line the
/// parse refuses lands in one bucket, declaring neither: not an object, `ns`
/// not a string, cut short mid-object.
#[derive(Debug, Default, PartialEq)]
pub struct Fields<'a> {
    pub namespace: Option<Cow<'a, str>>,
    pub severity: Option<Severity>,
}

impl<'a> Fields<'a> {
    pub fn of(line: &'a str) -> Fields<'a> {
        let Ok(keys) = serde_json::from_str::<Keys<'a>>(line) else {
            return Fields::default();
        };
        Fields {
            namespace: keys.ns.map(|Read(ns)| ns),
            severity: keys.sev.and_then(|Read(name)| name.parse().ok()),
        }
    }
}

pub fn select<'a>(config: &SelectConfig, line: &'a str) -> Selection<'a> {
    let fields = Fields::of(line);
    let kept = fields
        .severity
        .is_some_and(|severity| severity >= config.min_severity)
        || fields.namespace.is_some_and(|namespace| {
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
