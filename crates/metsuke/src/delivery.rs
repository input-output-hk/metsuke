//! Batch delivery: the only path from spooled samples to a sealed upload.
//! Owning the spool and the signing key makes the ADR-0002 ordering —
//! counter persisted before sealing, ack only for the rows that were
//! sealed — the only expressible call sequence, and keeps SQLite row ids
//! out of the main loop entirely.

use time::OffsetDateTime;

use crate::spool::{Spool, SpoolError};
use metsuke_wire::envelope::{self, Envelope, PoolId, SCHEMA_VERSION, Sample, SigningKey};

pub struct Delivery {
    spool: Spool,
    key: SigningKey,
    /// From config, not derived from `key`: a Calidus key's hash is not the
    /// pool id (ADR 0003).
    pool_id: PoolId,
    /// zstd level passed to `seal` (0 = zstd's default).
    compression_level: i32,
}

/// One sealed upload: the bytes to PUT and the signature to send with them.
/// The row ids it covers stay private, so the only rows an `ack` can delete
/// are the ones sealed into this batch, and consuming it prevents a double
/// ack.
pub struct SealedBatch {
    pub wire_bytes: Vec<u8>,
    pub signature: envelope::Signature,
    ids: Vec<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Seal(#[from] envelope::SealError),
}

impl Delivery {
    pub fn new(spool: Spool, key: SigningKey, pool_id: PoolId, compression_level: i32) -> Self {
        Delivery {
            spool,
            key,
            pool_id,
            compression_level,
        }
    }

    /// Append a sample to the spool.
    pub fn push(&mut self, sample: &Sample) -> Result<(), DeliveryError> {
        Ok(self.spool.push(sample)?)
    }

    /// Seal every outstanding sample into one signed batch, drawing (and
    /// persisting) the next replay counter. `None` when nothing is spooled.
    /// Rows stay spooled until `ack`; a retry after a failed PUT simply
    /// takes a fresh batch.
    pub fn take_batch(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        let rows = self.spool.outstanding()?;
        if rows.is_empty() {
            return Ok(None);
        }
        let counter = self.spool.next_counter()?;
        let (ids, samples) = rows.into_iter().map(|row| (row.id, row.sample)).unzip();
        let batch = Envelope {
            schema_version: SCHEMA_VERSION,
            pool_id: self.pool_id,
            agent_version: crate::AGENT_VERSION.to_string(),
            counter,
            timestamp: now,
            samples,
        };
        let (wire_bytes, signature) = envelope::seal(&self.key, &batch, self.compression_level)?;
        Ok(Some(SealedBatch {
            wire_bytes,
            signature,
            ids,
        }))
    }

    /// The server ACK'd this batch: delete exactly the rows it sealed.
    pub fn ack(&mut self, batch: SealedBatch) -> Result<(), DeliveryError> {
        Ok(self.spool.ack(&batch.ids)?)
    }
}
