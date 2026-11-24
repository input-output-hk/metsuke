//! What this server has accepted and refused, as the Prometheus text
//! exposition a scraper reads off `config::MetricsConfig::listen`.
//!
//! The journal says the same thing line by line, and a log pipeline under
//! pressure drops lines from every unit on a host at once; these counters are
//! what still says whether pools are reaching the archive when it does.
//!
//! Observation, not storage: ADR 0005 keeps the archive the only account of
//! what was accepted, and nothing here is read back to decide anything. The
//! counters start at zero with the process, so an accept followed by a restart
//! before the next scrape is never seen here, though it is in the archive.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use metsuke_wire::envelope::PoolId;
use time::OffsetDateTime;

use crate::applications::Codes;
use crate::archive::Kind;
use crate::refusal::Refusal;

/// The one route the metrics listener answers.
pub const PATH: &str = "/metrics";

/// The text exposition format, version 0.0.4, which is what every Prometheus
/// scraper reads without negotiating.
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// Every counter this server keeps. Built once from the allowlist and never
/// resized, so the series a scrape returns are fixed for the process's life
/// and a pool that has sent nothing reads zero rather than being absent.
pub struct Metrics {
    pools: BTreeMap<PoolId, Observed>,
    refused: [AtomicU64; Refusal::ALL.len()],
    /// `config::MetricsConfig::versions_per_pool`.
    versions_per_pool: NonZeroUsize,
}

/// What one allowlisted pool's submissions have added up to.
struct Observed {
    /// The pool as every sample labels it, encoded once.
    label: String,
    accepted: PerKind,
    accepted_bytes: PerKind,
    /// Each agent version this pool's submissions reported, against when the
    /// last of them landed.
    versions: Mutex<BTreeMap<String, Landed>>,
}

/// When a version last landed, told twice for two readers.
struct Landed {
    /// What the scrape shows. A timestamp rather than a count, because a
    /// count that reached one and stayed there is what `increase` cannot tell
    /// from no submission at all.
    archived_at: OffsetDateTime,
    /// What eviction orders by. Monotonic, and read under the lock, so the
    /// version that landed last is the latest here whatever the wall clock did.
    seen: Instant,
}

type PerKind = [AtomicU64; Kind::ALL.len()];

/// A counter family labelled by pool and kind, and what it reads off a pool.
struct PerKindFamily {
    name: &'static str,
    help: &'static str,
    counters: fn(&Observed) -> &PerKind,
}

const PER_KIND: [PerKindFamily; 2] = [
    PerKindFamily {
        name: "metsuke_submissions_accepted_total",
        help: "Submissions archived, by the pool whose key signed them and the kind they carry.",
        counters: |observed| &observed.accepted,
    },
    PerKindFamily {
        name: "metsuke_submission_bytes_total",
        help: "Bytes of submissions archived, as signed, by pool and kind.",
        counters: |observed| &observed.accepted_bytes,
    },
];

impl Metrics {
    pub fn new(allowlist: &Codes, versions_per_pool: NonZeroUsize) -> Metrics {
        let observed = |pool_id: &PoolId| Observed {
            label: pool_id.to_bech32(),
            accepted: std::array::from_fn(|_| AtomicU64::new(0)),
            accepted_bytes: std::array::from_fn(|_| AtomicU64::new(0)),
            versions: Mutex::new(BTreeMap::new()),
        };
        Metrics {
            pools: allowlist
                .keys()
                .map(|pool_id| (*pool_id, observed(pool_id)))
                .collect(),
            refused: std::array::from_fn(|_| AtomicU64::new(0)),
            versions_per_pool,
        }
    }

    /// One submission archived. The pool is one the allowlist admitted, and
    /// these counters were built from that allowlist, so a pool without them
    /// is this server contradicting itself. `version` is the agent's account
    /// of itself, already bounded (`metsuke_wire::http::one_line`), so two raw
    /// versions that bound to the same text are one series. `archived_at` is
    /// read once the archive write returned, and only ever moves a version's
    /// time forward, so two submissions finishing out of order cannot set it
    /// back. Past `versions_per_pool`, the version that landed longest ago is
    /// dropped, by `Landed::seen`.
    pub fn accepted(
        &self,
        pool_id: PoolId,
        kind: Kind,
        bytes: usize,
        version: String,
        archived_at: OffsetDateTime,
    ) {
        let observed = self
            .pools
            .get(&pool_id)
            .expect("counters exist for every pool the allowlist admits");
        observed.accepted[kind as usize].fetch_add(1, Ordering::Relaxed);
        observed.accepted_bytes[kind as usize].fetch_add(bytes as u64, Ordering::Relaxed);
        let mut versions = observed.versions.lock().expect("no panic holds this lock");
        let seen = Instant::now();
        let landed = versions
            .entry(version)
            .or_insert(Landed { archived_at, seen });
        landed.archived_at = landed.archived_at.max(archived_at);
        landed.seen = seen;
        if versions.len() > self.versions_per_pool.get() {
            let stalest = versions
                .iter()
                .min_by_key(|(_, landed)| landed.seen)
                .map(|(version, _)| version.clone())
                .expect("a map over its bound holds an entry");
            versions.remove(&stalest);
        }
    }

    pub fn refused(&self, refusal: Refusal) {
        self.refused[refusal as usize].fetch_add(1, Ordering::Relaxed);
    }

    /// The exposition, one family after another, each under its HELP and TYPE.
    /// Only the version is escaped: it is the one label value an agent wrote,
    /// and pool, kind and reason are this server's own ASCII.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for PerKindFamily {
            name,
            help,
            counters,
        } in PER_KIND
        {
            family(&mut out, name, "counter", help);
            for observed in self.pools.values() {
                for kind in Kind::ALL {
                    let value = counters(observed)[kind as usize].load(Ordering::Relaxed);
                    let pool = &observed.label;
                    emit(
                        &mut out,
                        format_args!("{name}{{pool=\"{pool}\",kind=\"{kind}\"}} {value}"),
                    );
                }
            }
        }
        let name = "metsuke_submissions_refused_total";
        family(&mut out, name, "counter", "Submissions refused, by reason.");
        for refusal in Refusal::ALL {
            let value = self.refused[*refusal as usize].load(Ordering::Relaxed);
            let reason = refusal.label();
            emit(
                &mut out,
                format_args!("{name}{{reason=\"{reason}\"}} {value}"),
            );
        }
        let name = "metsuke_agent_last_accepted_timestamp_seconds";
        family(
            &mut out,
            name,
            "gauge",
            "When a submission reporting this agent version was last archived for this pool.",
        );
        for observed in self.pools.values() {
            let pool = &observed.label;
            let versions = observed.versions.lock().expect("no panic holds this lock");
            for (version, landed) in versions.iter() {
                let version = escaped(version);
                let seconds = landed.archived_at.unix_timestamp_nanos() as f64 / 1e9;
                emit(
                    &mut out,
                    format_args!("{name}{{pool=\"{pool}\",version=\"{version}\"}} {seconds}"),
                );
            }
        }
        out
    }
}

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    emit(out, format_args!("# HELP {name} {help}"));
    emit(out, format_args!("# TYPE {name} {kind}"));
}

/// One exposition line.
fn emit(out: &mut String, line: std::fmt::Arguments<'_>) {
    out.write_fmt(line)
        .expect("writing to a String cannot fail");
    out.push('\n');
}

/// A label value as the text format quotes it: backslash, double quote and
/// line feed are the three it escapes.
fn escaped(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
