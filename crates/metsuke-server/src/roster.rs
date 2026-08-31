//! The Key Roster: which **Leios Keys** the chain registers for each pool
//! (ADR 0011). A file something outside this repository writes off
//! `cardano-cli query pool-state`; this server reads it, re-reads it when it
//! changes, and queries nothing itself.
//!
//! A pool's entry holds every key the roster's writer saw for it, the
//! registered one and any announced for the next epoch together. ADR 0011 has
//! why.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use serde::Deserialize;

use metsuke_wire::envelope::{PoolId, PoolIdError};
use metsuke_wire::hex::{self, HexError};
use metsuke_wire::journal::{INFO, WARNING};
use metsuke_wire::leios::{LeiosKeyError, LeiosPublicKey, PUBLIC_KEY_BYTES};

/// The file as written: the chain position it was taken at, so a roster that
/// has stopped being updated is diagnosable rather than silently wrong, and
/// the keys themselves.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterFile {
    epoch: u64,
    slot: u64,
    /// Both keyed and valued in the hex `pool-state` answers with: the file is
    /// a transcription of the chain's answer, so the one place a pool id is
    /// converted stays `PoolId` (ADR 0011).
    pools: HashMap<String, Vec<String>>,
}

/// A roster that could not be read, and the file it was read from. The path is
/// stated here rather than in every reason: one file is being read, and what
/// varies is why it did not.
#[derive(Debug, thiserror::Error)]
#[error("the Leios key roster {path} {kind}")]
pub struct RosterError {
    path: String,
    #[source]
    kind: Unreadable,
}

#[derive(Debug, thiserror::Error)]
enum Unreadable {
    #[error("cannot be read: {0}")]
    Read(#[source] io::Error),
    #[error("is not the JSON this build reads: {0}")]
    Parse(#[source] serde_json::Error),
    // Boxed: a bech32 failure is the widest thing this enum can hold, and it
    // would otherwise be the size of every roster read that succeeds.
    #[error("is keyed by {found:?}, which is not a pool id: {source}")]
    NotAPoolId {
        found: String,
        #[source]
        source: Box<PoolIdError>,
    },
    #[error("lists a key {at} for pool {pool_id} that is not hex: {source}")]
    NotHex {
        pool_id: PoolId,
        at: usize,
        #[source]
        source: HexError,
    },
    #[error("lists a key {at} for pool {pool_id} that is no key: {source}")]
    NotAKey {
        pool_id: PoolId,
        at: usize,
        #[source]
        source: LeiosKeyError,
    },
}

/// What the roster held when it was last read, and what it was read from, so a
/// change is noticed without watching the file.
struct Loaded {
    epoch: u64,
    slot: u64,
    pools: HashMap<PoolId, Vec<LeiosPublicKey>>,
    read_from: Option<Stamp>,
}

/// Enough of the file's metadata to say it is not the one already read. Both
/// halves, because a rewrite within the timestamp's resolution can still
/// change the length.
#[derive(PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

impl Stamp {
    /// `None` where the file cannot be stated, which `refresh` reads as "try a
    /// full read and let that report the reason".
    fn of(path: &Path) -> Option<Stamp> {
        let metadata = fs::metadata(path).ok()?;
        Some(Stamp {
            modified: metadata.modified().ok()?,
            len: metadata.len(),
        })
    }
}

pub struct Roster {
    path: PathBuf,
    loaded: RwLock<Loaded>,
}

impl std::fmt::Debug for Roster {
    /// Where it came from and what it was taken at. Which keys it holds is a
    /// list as long as the allowlist, and no line wants that.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (epoch, slot) = self.position();
        f.debug_struct("Roster")
            .field("path", &self.path)
            .field("epoch", &epoch)
            .field("slot", &slot)
            .field("pools", &self.read().pools.len())
            .finish()
    }
}

impl Roster {
    /// Read it once, loudly: a server told to take Leios-key submissions
    /// against a roster it cannot read starts and refuses every pool.
    pub fn load(path: &Path) -> Result<Roster, RosterError> {
        let roster = Roster {
            path: path.to_path_buf(),
            loaded: RwLock::new(Loaded {
                epoch: 0,
                slot: 0,
                pools: HashMap::new(),
                read_from: None,
            }),
        };
        roster.reread()?;
        Ok(roster)
    }

    /// Whether the chain registers `key` for `pool_id`, as of the roster this
    /// server has. The lookup is what a claimed pool id is believed on, so it
    /// answers only about the pool that was claimed.
    pub fn registers(&self, pool_id: PoolId, key: &LeiosPublicKey) -> bool {
        self.refresh();
        self.read()
            .pools
            .get(&pool_id)
            .is_some_and(|keys| keys.contains(key))
    }

    /// The chain position the roster was taken at, for the line a start-up or
    /// a reload logs.
    pub fn position(&self) -> (u64, u64) {
        let loaded = self.read();
        (loaded.epoch, loaded.slot)
    }

    /// Re-read where the file on disk is not the one already read. A failed
    /// re-read keeps what is loaded and says so: emptying the roster because a
    /// writer was halfway through writing it would refuse every pool.
    fn refresh(&self) {
        let stamp = Stamp::of(&self.path);
        if stamp.is_some() && stamp == self.read().read_from {
            return;
        }
        match self.reread() {
            Ok(()) => {
                let (epoch, slot) = self.position();
                eprintln!(
                    "{INFO}reloaded the Leios key roster {}, taken in epoch {epoch} at slot {slot}",
                    self.path.display()
                );
            }
            Err(error) => eprintln!("{WARNING}{error}; keeping the roster already loaded"),
        }
    }

    fn reread(&self) -> Result<(), RosterError> {
        // The stamp is taken before the read, so a write that lands during it
        // leaves a stamp that does not match and is picked up next time.
        let read_from = Stamp::of(&self.path);
        let loaded = self.read_file().map_err(|kind| RosterError {
            path: self.path.display().to_string(),
            kind,
        })?;
        *self.write() = Loaded {
            read_from,
            ..loaded
        };
        Ok(())
    }

    /// The file's own reading, which names no path: every reason it fails is
    /// about the same one file, and `reread` states it once.
    fn read_file(&self) -> Result<Loaded, Unreadable> {
        let text = fs::read_to_string(&self.path).map_err(Unreadable::Read)?;
        let file: RosterFile = serde_json::from_str(&text).map_err(Unreadable::Parse)?;
        let mut pools = HashMap::with_capacity(file.pools.len());
        for (hex, keys) in file.pools {
            let pool_id = PoolId::from_hex(&hex).map_err(|source| Unreadable::NotAPoolId {
                found: hex,
                source: Box::new(source),
            })?;
            let keys = keys
                .iter()
                .enumerate()
                .map(|(at, text)| {
                    let bytes = hex::decode::<PUBLIC_KEY_BYTES>(text).map_err(|source| {
                        Unreadable::NotHex {
                            pool_id,
                            at,
                            source,
                        }
                    })?;
                    LeiosPublicKey::from_bytes(&bytes).map_err(|source| Unreadable::NotAKey {
                        pool_id,
                        at,
                        source,
                    })
                })
                .collect::<Result<Vec<_>, Unreadable>>()?;
            pools.insert(pool_id, keys);
        }
        Ok(Loaded {
            epoch: file.epoch,
            slot: file.slot,
            pools,
            read_from: None,
        })
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Loaded> {
        self.loaded
            .read()
            .expect("the roster lock is never poisoned: reading cannot panic")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Loaded> {
        self.loaded
            .write()
            .expect("the roster lock is never poisoned: replacing cannot panic")
    }
}
