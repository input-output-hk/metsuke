//! What an object key encodes. The server writes keys and the fetch tool reads
//! them, so the format lives here: `parse` is the inverse of `to_key`, and
//! nothing else parses a key.

use time::{Date, OffsetDateTime};
use uuid::{NoContext, Timestamp, Uuid, Version};

use crate::envelope::{
    AgentId, AgentIdError, PoolId, SCHEMA_VERSION_LINES, SCHEMA_VERSION_SCRAPES,
};

pub const KEY_PREFIX: &str = "v1/";

/// The suffix every object carries: JSON Lines, zstd-compressed, as the wire
/// container holds them.
pub const KEY_SUFFIX: &str = ".jsonl.zst";

/// Which payload a submission carries, as the object key spells it. Named from the
/// schema version rather than read out of the payload: the server never
/// decompresses one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Metrics,
    Logs,
}

impl Kind {
    /// `None` for a version this build has no name for. A submission it cannot name
    /// is a submission it cannot file, which is the only reason ingest still reads
    /// the version at all.
    pub fn of(schema_version: u32) -> Option<Kind> {
        match schema_version {
            SCHEMA_VERSION_SCRAPES => Some(Kind::Metrics),
            SCHEMA_VERSION_LINES => Some(Kind::Logs),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Metrics => "metrics",
            Kind::Logs => "logs",
        }
    }

    pub fn parse(segment: &str) -> Option<Kind> {
        [Kind::Metrics, Kind::Logs]
            .into_iter()
            .find(|kind| kind.as_str() == segment)
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// As the key segment spells it, so what a consumer writes down is what it
/// would have read (`metsuke_fetch::cursor`).
impl serde::Serialize for Kind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for Kind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Kind, D::Error> {
        let segment = <String as serde::Deserialize>::deserialize(deserializer)?;
        Kind::parse(&segment)
            .ok_or_else(|| serde::de::Error::custom(format!("{segment:?} is not a payload kind")))
    }
}

/// Time-major, so one start-after cursor is the whole delta-sync protocol: the
/// day folder orders the corpus and the UUIDv7 orders within it. Uniqueness is
/// the UUIDv7's alone, never the clock, the sequence number or the agent id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectName {
    /// Stamped when the submission was received, which is what makes a late
    /// spool flush sort after everything already synced.
    pub id: Uuid,
    pub pool_id: PoolId,
    pub agent_id: AgentId,
    pub kind: Kind,
}

#[derive(Debug, thiserror::Error)]
#[error("{key:?} is not a v1 archive object key: {reason}")]
pub struct ObjectNameError {
    key: String,
    reason: String,
}

impl ObjectName {
    /// The name a submission received at `now` is filed under.
    pub fn stamped(
        now: OffsetDateTime,
        pool_id: PoolId,
        agent_id: AgentId,
        kind: Kind,
    ) -> ObjectName {
        // Seconds and nanos of the receipt instant: the UUIDv7 carries the
        // millisecond, which is what the day folder is then read back from.
        let stamp = Timestamp::from_unix(NoContext, now.unix_timestamp() as u64, now.nanosecond());
        ObjectName {
            id: Uuid::new_v7(stamp),
            pool_id,
            agent_id,
            kind,
        }
    }

    /// The day the id was stamped on, UTC. Read back out of the id rather than
    /// held beside it, so the folder and the object cannot name two days.
    pub fn date(&self) -> Date {
        let (seconds, _) = self
            .id
            .get_timestamp()
            .expect("a v7 id carries its timestamp")
            .to_unix();
        // UTC, not the sender's offset: a key naming a local day would sort two
        // pools' uploads of the same instant into different folders. A v7
        // timestamp is a millisecond count, so every id this stamps is inside
        // `OffsetDateTime`'s range.
        OffsetDateTime::from_unix_timestamp(seconds as i64)
            .expect("a v7 millisecond is a representable instant")
            .date()
    }

    pub fn to_key(&self) -> String {
        let date = self.date();
        format!(
            "{KEY_PREFIX}{year:04}-{month:02}-{day:02}/{id}-{pool}-{agent}-{kind}{KEY_SUFFIX}",
            year = date.year(),
            month = u8::from(date.month()),
            day = date.day(),
            id = self.id,
            pool = self.pool_id,
            agent = self.agent_id,
            kind = self.kind,
        )
    }

    pub fn parse(key: &str) -> Result<ObjectName, ObjectNameError> {
        let refuse = |reason: String| ObjectNameError {
            key: key.to_string(),
            reason,
        };
        let [prefix, date, file] = *key.split('/').collect::<Vec<_>>() else {
            return Err(refuse("expected v1/<date>/<file>".to_string()));
        };
        let schema = KEY_PREFIX.trim_end_matches('/');
        if prefix != schema {
            return Err(refuse(format!(
                "schema prefix is {prefix:?}, not {schema:?}"
            )));
        }
        let stem = file
            .strip_suffix(KEY_SUFFIX)
            .ok_or_else(|| refuse(format!("{file:?} is not a {KEY_SUFFIX} object")))?;
        // Split from the left past the id, whose own dashes are at fixed
        // positions, then from the right for the kind: an agent id holds dashes
        // too, and a pool id holds none.
        let (id, rest) = stem
            .split_at_checked(uuid::fmt::Hyphenated::LENGTH)
            .ok_or_else(|| refuse(format!("{stem:?} is shorter than a uuid")))?;
        let id = Uuid::try_parse(id).map_err(|error| refuse(format!("id {id:?}: {error}")))?;
        // `date()` reads the day out of the id, which only a v7 carries; any
        // other version has to be refused here or it panics there.
        if id.get_version() != Some(Version::SortRand) {
            return Err(refuse(format!("id {id} is not a uuid v7")));
        }
        let rest = rest
            .strip_prefix('-')
            .ok_or_else(|| refuse(format!("{stem:?} is not <id>-<pool>-<agent>-<kind>")))?;
        let (pool, rest) = rest
            .split_once('-')
            .ok_or_else(|| refuse(format!("{rest:?} is not <pool>-<agent>-<kind>")))?;
        let (agent, kind) = rest
            .rsplit_once('-')
            .ok_or_else(|| refuse(format!("{rest:?} is not <agent>-<kind>")))?;
        let name = ObjectName {
            id,
            pool_id: PoolId::from_bech32(pool).map_err(|error| refuse(error.to_string()))?,
            agent_id: AgentId::parse(agent)
                .map_err(|error: AgentIdError| refuse(error.to_string()))?,
            kind: Kind::parse(kind)
                .ok_or_else(|| refuse(format!("{kind:?} is not a payload kind")))?,
        };
        // The folder repeats the id's day, and the id is the only version this
        // reads a day out of; a key where the two disagree was written by
        // something other than `to_key`.
        if name.to_key() != key {
            return Err(refuse(format!(
                "the {date} folder is not where {id} was stamped"
            )));
        }
        Ok(name)
    }
}
