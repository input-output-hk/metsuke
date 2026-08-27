//! Which of a node's trace lines the agent ships: a namespace prefix list,
//! configuration, and nothing else. Why severity is not a second rule: ADR 0010.
//!
//! A line is parsed as a JSON object and only `ns` is read off it. That parse is
//! what a selected line is from here on (`envelope::TraceLine`).

use metsuke_wire::envelope::{TraceLine, TraceLineError};

/// The rules in force, which only `new` builds.
#[derive(Debug)]
pub struct SelectConfig {
    namespaces: Vec<String>,
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
    /// (metsuke-4zo.99).
    pub fn new(roots: &[String], namespaces: Vec<String>) -> Result<SelectConfig, OutsideRoots> {
        for namespace in &namespaces {
            if !roots.iter().any(|root| is_under(namespace, root)) {
                return Err(OutsideRoots {
                    namespace: namespace.clone(),
                    roots: roots.join(", "),
                });
            }
        }
        Ok(SelectConfig { namespaces })
    }
}

/// What one line is. The node prints its whole `NodeConfiguration` before the
/// tracing system is up, so a line declaring no namespace is a normal thing to
/// meet once per node start and is skipped like any other unwanted line.
#[derive(Debug, PartialEq)]
pub enum Selection {
    Ship(TraceLine),
    Skip,
    /// A well-formed line refused only for declaring the reserved
    /// `PROVENANCE_KEY` at its top level (`TraceLineError::ReservedKey`), kept
    /// apart from `Skip` so the caller can count and log it instead of
    /// dropping it unremarked.
    ReservedKey,
}

/// What one line declares, of the field a rule reads, off the object's own top
/// level so a `data` payload carrying it is not a candidate.
#[derive(Debug, PartialEq)]
pub struct Fields<'a> {
    pub namespace: Option<&'a str>,
}

impl<'a> Fields<'a> {
    pub fn of(line: &'a TraceLine) -> Fields<'a> {
        Fields {
            namespace: line.get("ns").and_then(serde_json::Value::as_str),
        }
    }
}

/// Whether `candidate` is `prefix` or sits under it, by segment: `Consensus.Leios`
/// reaches neither `Consensus.LeiosPeer` nor anything else, so a rule names a
/// namespace the node emits or it selects nothing.
fn is_under(candidate: &str, prefix: &str) -> bool {
    candidate
        .strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

pub fn select(config: &SelectConfig, line: &str) -> Selection {
    let line = match TraceLine::parse(line) {
        Ok(line) => line,
        Err(TraceLineError::ReservedKey) => return Selection::ReservedKey,
        Err(TraceLineError::NotAnObject(_)) => return Selection::Skip,
    };
    let kept = Fields::of(&line).namespace.is_some_and(|namespace| {
        config
            .namespaces
            .iter()
            .any(|rule| is_under(namespace, rule))
    });
    match kept {
        true => Selection::Ship(line),
        false => Selection::Skip,
    }
}
