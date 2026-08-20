//! Delivery seam tests (ticket metsuke-4zo.4): spool state in, sealed bytes
//! out, ack shrinks the spool. The batch is checked by `open`ing it with the
//! verifying key — the same call the server makes.

use metsuke::delivery::Delivery;
use metsuke::envelope::{self, PoolId, Sample, SigningKey};
use metsuke::spool::{Spool, SpoolConfig};
use time::OffsetDateTime;

// Large enough for any test batch; the real limit is server config.
const TEST_DECOMPRESS_LIMIT: u64 = 64 * 1024 * 1024;

fn sample_at(unix_secs: i64) -> Sample {
    Sample {
        sampled_at: OffsetDateTime::from_unix_timestamp(unix_secs).unwrap(),
        block_height: Some(unix_secs as u64),
        slot: None,
        slot_in_epoch: None,
        epoch: None,
        sync_progress: None,
        node_version: None,
        node_revision: None,
        clock_offset_ms: None,
    }
}

fn temp_delivery(dir: &tempfile::TempDir, key: &SigningKey) -> Delivery {
    let spool = Spool::open(&SpoolConfig {
        path: dir.path().join("spool.sqlite"),
        max_samples: 100,
    })
    .unwrap();
    Delivery::new(
        spool,
        key.clone(),
        PoolId::from_cold_key(&key.verifying_key()),
        0,
    )
}

// The whole loop contract: what was pushed comes out as a batch the server's
// own call (`open` with the verifying key) accepts, and an ack empties the
// spool.
#[test]
fn pushed_samples_seal_verify_and_ack_drains_the_spool() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    let samples = [sample_at(1), sample_at(2)];
    for sample in &samples {
        delivery.push(sample).unwrap();
    }
    let batch = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let opened = envelope::open(
        &key.verifying_key(),
        &batch.wire_bytes,
        &batch.signature,
        TEST_DECOMPRESS_LIMIT,
    )
    .unwrap();
    assert_eq!(opened.samples, samples);
    assert_eq!(opened.pool_id, PoolId::from_cold_key(&key.verifying_key()));
    delivery.ack(batch).unwrap();
    assert!(
        delivery
            .take_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}

// A failed PUT means no ack: the retry offers the same samples again but
// under a fresh counter — a counter value is never handed out twice
// (ADR 0002).
#[test]
fn unacked_batch_is_retaken_with_a_fresh_counter() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    delivery.push(&sample_at(1)).unwrap();
    let open = |batch: &metsuke::delivery::SealedBatch| {
        envelope::open(
            &key.verifying_key(),
            &batch.wire_bytes,
            &batch.signature,
            TEST_DECOMPRESS_LIMIT,
        )
        .unwrap()
    };
    let first = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let retry = delivery
        .take_batch(OffsetDateTime::UNIX_EPOCH)
        .unwrap()
        .unwrap();
    let (first, retry) = (open(&first), open(&retry));
    assert_eq!(first.samples, retry.samples);
    assert!(retry.counter > first.counter);
}

#[test]
fn empty_spool_yields_no_batch() {
    let dir = tempfile::tempdir().unwrap();
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut delivery = temp_delivery(&dir, &key);
    assert!(
        delivery
            .take_batch(OffsetDateTime::UNIX_EPOCH)
            .unwrap()
            .is_none()
    );
}
