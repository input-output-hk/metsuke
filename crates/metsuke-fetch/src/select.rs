//! Which of the listed keys a run is about, past the prefix the server filters
//! on. The pool, the agent and the kind are segments of the key
//! (`metsuke_wire::key::ObjectName`) and sit after the id, so no literal prefix
//! selects them. Read off the key here, they still cost no download.

use metsuke_wire::envelope::{AgentId, PoolId};
use metsuke_wire::key::{Kind, ObjectName};
use serde::{Deserialize, Serialize};

/// Which keys a run is about: what the server filters the listing on, what
/// this side selects out of what came back, and the days it is bounded to.
pub struct Filters<'a> {
    /// The literal head of a key. An empty one is normalized to the archive's
    /// own prefix before it reaches here (`cli::Args`).
    pub prefix: &'a str,
    pub selection: &'a Selection,
    pub days: &'a Days,
}

/// An inclusive range of days, as the key boundaries it means. Dates are
/// parsed in `cli`, so nothing downstream handles one: a day folder is derived
/// from its objects' own timestamps (`metsuke_wire::key::ObjectName::date`),
/// which is what makes a key range and a time range the same thing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Days {
    /// Exclusive `start-after` for an inclusive first day. `v1/<day>` carries
    /// no trailing slash on purpose: it sorts below that day's first key and
    /// above every key of the day before.
    pub from: Option<String>,
    /// Exclusive upper bound for an inclusive last day, so `v1/<the day
    /// after>`. `None` where the run has no last day.
    pub until: Option<String>,
}

impl Days {
    /// Where the listing starts, which is the later of the cursor and the
    /// first day: a resumed run must not walk back to the bound, and a fresh
    /// one must not start before it.
    pub fn after(&self, resuming: &str) -> String {
        match &self.from {
            Some(from) if from.as_str() > resuming => from.clone(),
            _ => resuming.to_string(),
        }
    }

    /// Whether `key` is past the last day. The walk stops at the first of
    /// these rather than filtering, because keys arrive in order.
    pub fn past(&self, key: &str) -> bool {
        self.until
            .as_deref()
            .is_some_and(|until| key.as_bytes() >= until.as_bytes())
    }
}

/// Empty selects everything, which is what a run with no filter flags is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub pool: Option<PoolId>,
    pub agent: Option<AgentId>,
    pub kind: Option<Kind>,
}

impl std::fmt::Display for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named = [
            self.pool.as_ref().map(|pool| format!("pool {pool}")),
            self.agent.as_ref().map(|agent| format!("agent {agent}")),
            self.kind.map(|kind| format!("kind {kind}")),
        ];
        match named.iter().flatten().cloned().collect::<Vec<String>>() {
            filters if filters.is_empty() => f.write_str("every pool, agent and kind"),
            filters => f.write_str(&filters.join(", ")),
        }
    }
}

/// What a key is to a selection. `Unnameable` is its own answer rather than a
/// `No`: it is not the filters passing over an object but this build having no
/// reading of the key at all, and the download route refuses such a key anyway
/// (`metsuke_server::archive::FilesystemArchive::reader`), so a sync that took
/// it as an object would stall at it every run.
#[derive(Debug, PartialEq)]
pub enum Selected {
    Yes,
    No,
    Unnameable,
}

impl Selection {
    /// Every key is parsed, filters or none: what cannot be named cannot be
    /// synced, and the count is what says the archive holds such objects.
    pub fn selects(&self, key: &str) -> Selected {
        let Ok(name) = ObjectName::parse(key) else {
            return Selected::Unnameable;
        };
        match self.pool.as_ref().is_none_or(|pool| *pool == name.pool_id)
            && self
                .agent
                .as_ref()
                .is_none_or(|agent| *agent == name.agent_id)
            && self.kind.is_none_or(|kind| kind == name.kind)
        {
            true => Selected::Yes,
            false => Selected::No,
        }
    }
}
