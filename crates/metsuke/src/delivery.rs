//! Batch delivery: the only path from spooled rows to a sealed upload.
//! Owning the spool and the signing key makes the ADR-0002 ordering —
//! counter persisted before sealing, ack only for the rows that were
//! sealed — the only expressible call sequence, and keeps SQLite row ids
//! out of the main loop entirely.
//!
//! Samples and trace lines seal into separate envelopes because a schema
//! version names one payload shape (`envelope::Payload`). Both are taken from
//! here, on one thread, so the two never race for a replay counter.

use time::OffsetDateTime;

use crate::spool::{Spool, SpoolError, UncarriableReport};
use metsuke_wire::envelope::{self, Envelope, Payload, PoolId, Sample, SigningKey};

pub struct Delivery {
    spool: Spool,
    key: SigningKey,
    /// From config, not derived from `key`: a Calidus key's hash is not the
    /// pool id (ADR 0003).
    pool_id: PoolId,
    /// zstd level passed to `seal` (0 = zstd's default).
    compression_level: i32,
    /// Pre-compression ceiling on one envelope: its header frame plus its
    /// payload lines. The server bounds the two separately, and the payload
    /// half is what its `max_decompressed_bytes` bounds — so a batch under this
    /// is under that whenever the two numbers agree. It is still the agent's
    /// own number: nothing in the wire contract lets it discover the server's,
    /// so a batch over that is rejected at upload and stays spooled.
    batch_max_bytes: u64,
}

/// One sealed upload: the bytes to PUT and the signature to send with them.
/// The rows it covers stay private, so the only rows an `ack` can delete are
/// the ones sealed into this batch, and consuming it prevents a double ack.
pub struct SealedBatch {
    pub wire_bytes: Vec<u8>,
    pub signature: envelope::Signature,
    rows: BatchRows,
}

/// Which stream's rows a batch drew from, so `ack` deletes from the table it
/// sealed rather than from whichever one the caller remembers.
enum BatchRows {
    Samples(Vec<i64>),
    Lines(Vec<i64>),
}

#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    #[error(transparent)]
    Spool(#[from] SpoolError),
    #[error(transparent)]
    Seal(#[from] envelope::SealError),
    #[error(
        "upload_batch_max_bytes is {batch_max_bytes}, which leaves no room for a row \
         beside this envelope's {framing} bytes of framing"
    )]
    BudgetBelowFraming { batch_max_bytes: u64, framing: u64 },
}

impl Delivery {
    pub fn new(
        spool: Spool,
        key: SigningKey,
        pool_id: PoolId,
        compression_level: i32,
        batch_max_bytes: u64,
    ) -> Self {
        Delivery {
            spool,
            key,
            pool_id,
            compression_level,
            batch_max_bytes,
        }
    }

    /// Append a sample to the spool. Returns how many rows the spool's cap
    /// dropped to make room.
    pub fn push(&mut self, sample: &Sample) -> Result<u64, DeliveryError> {
        Ok(self.spool.push(sample)?)
    }

    /// Seal one batch of outstanding samples, drawing (and persisting) the
    /// next replay counter. `None` when nothing is spooled. Rows stay spooled
    /// until `ack`; a retry after a failed PUT simply takes a fresh batch.
    pub fn take_batch(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        let rows = self
            .spool
            .outstanding(self.row_budget(now, Payload::Samples { samples: vec![] })?)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let (ids, samples) = rows.into_iter().map(|row| (row.id, row.sample)).unzip();
        self.seal(now, Payload::Samples { samples }, BatchRows::Samples(ids))
            .map(Some)
    }

    /// The same for outstanding trace lines.
    pub fn take_line_batch(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        let rows = self
            .spool
            .outstanding_lines(self.row_budget(now, Payload::Lines { lines: vec![] })?)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let (ids, lines) = rows.into_iter().map(|row| (row.id, row.line)).unzip();
        self.seal(now, Payload::Lines { lines }, BatchRows::Lines(ids))
            .map(Some)
    }

    /// What the rows may sum to: the budget less what this envelope's header
    /// frame spends, measured by building that frame rather than by a second
    /// account of its fields. `u64::MAX` is the widest counter this pool can
    /// ever draw, so the reserve never comes up short of the counter the batch
    /// is actually stamped with.
    ///
    /// A budget the framing already exhausts is an error, not a zero: every
    /// row is over a zero budget, so offering one would have the spool drop the
    /// head row a tick for as long as the config stands
    /// (`spool::outstanding_rows`).
    fn row_budget(&self, now: OffsetDateTime, empty: Payload) -> Result<u64, DeliveryError> {
        let header = envelope::header_json(&self.envelope(u64::MAX, now, empty))?;
        let framing = (envelope::HEADER_OFFSET + header.len()) as u64;
        match self.batch_max_bytes.checked_sub(framing) {
            Some(budget) if budget > 0 => Ok(budget),
            _ => Err(DeliveryError::BudgetBelowFraming {
                batch_max_bytes: self.batch_max_bytes,
                framing,
            }),
        }
    }

    fn envelope(&self, counter: u64, now: OffsetDateTime, payload: Payload) -> Envelope {
        Envelope::new(
            self.pool_id,
            crate::AGENT_VERSION.to_string(),
            counter,
            now,
            payload,
        )
    }

    fn seal(
        &mut self,
        now: OffsetDateTime,
        payload: Payload,
        rows: BatchRows,
    ) -> Result<SealedBatch, DeliveryError> {
        let counter = self.spool.next_counter()?;
        let batch = self.envelope(counter, now, payload);
        let (wire_bytes, signature) = envelope::seal(&self.key, &batch, self.compression_level)?;
        Ok(SealedBatch {
            wire_bytes,
            signature,
            rows,
        })
    }

    /// What the last batches dropped for being larger on their own than one
    /// batch's budget (`spool::Spool::take_uncarriable_report`).
    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        self.spool.take_uncarriable_report()
    }

    /// The server ACK'd this batch: delete exactly the rows it sealed.
    pub fn ack(&mut self, batch: SealedBatch) -> Result<(), DeliveryError> {
        match batch.rows {
            BatchRows::Samples(ids) => self.spool.ack(&ids)?,
            BatchRows::Lines(ids) => self.spool.ack_lines(&ids)?,
        }
        Ok(())
    }
}
