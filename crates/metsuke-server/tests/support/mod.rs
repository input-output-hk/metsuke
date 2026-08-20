//! Helpers for the ingest tests: keys, an envelope, and the sealed form of
//! one.

use metsuke::envelope::{
    self, Envelope, PoolId, SCHEMA_VERSION, Sample, Signature, SigningKey, VerifyingKey,
};
use metsuke_server::config::IngestConfig;
use metsuke_server::intake::Submission;
use time::OffsetDateTime;

/// The all-sevens test seed, matching the agent suite.
pub fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// A second key, for the pool that did not sign.
pub fn other_key() -> SigningKey {
    SigningKey::from_bytes(&[9u8; 32])
}

pub fn pool_of(key: &SigningKey) -> PoolId {
    PoolId::from_cold_key(&key.verifying_key())
}

/// The clock every test judges against; envelopes are stamped with it.
pub fn test_now() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_755_000_000).unwrap()
}

pub fn envelope_for(key: &SigningKey, counter: u64) -> Envelope {
    Envelope {
        schema_version: SCHEMA_VERSION,
        pool_id: pool_of(key),
        agent_version: metsuke::AGENT_VERSION.to_string(),
        counter,
        timestamp: test_now(),
        samples: vec![Sample {
            sampled_at: test_now(),
            block_height: Some(12_345),
            slot: None,
            slot_in_epoch: None,
            epoch: None,
            sync_progress: None,
            node_version: None,
            node_revision: None,
            clock_offset_ms: None,
        }],
    }
}

/// The wire bytes and signature a client would send for this envelope.
pub fn seal(key: &SigningKey, envelope: &Envelope) -> (Vec<u8>, Signature) {
    envelope::seal(key, envelope, 0).unwrap()
}

/// Assemble the headers-and-body triple the intake takes.
pub fn submission<'a>(
    vkey: VerifyingKey,
    pool_id: PoolId,
    signature: Signature,
    wire_bytes: &'a [u8],
) -> Submission<'a> {
    Submission {
        pool_id,
        vkey,
        signature,
        wire_bytes,
    }
}

/// Limits wide enough that only the check under test can fire.
pub fn permissive_config(allowed: &[PoolId]) -> IngestConfig {
    IngestConfig {
        allowlist: allowed.iter().copied().collect(),
        max_body_bytes: 1024 * 1024,
        max_decompressed_bytes: 4 * 1024 * 1024,
        rate_limit_uploads: 100,
        rate_limit_window_secs: 3600,
        max_timestamp_skew_secs: 300,
    }
}
