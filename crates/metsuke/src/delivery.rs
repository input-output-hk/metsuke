//! Batch delivery: the only path from spooled rows to a sealed upload.
//! Owning the spool and the signing key makes one ordering (counter persisted
//! before sealing, ack only for the rows that were sealed) the only
//! expressible call sequence, and keeps SQLite row ids out of the main loop
//! entirely.
//!
//! Scrapes and trace lines seal into separate envelopes because a schema
//! version names one payload shape (`envelope::Payload`). Both are taken from
//! here, on one thread, so the two never race for a counter.

use time::OffsetDateTime;

use crate::spool::{RowBudget, Spool, SpoolError, SpooledRow, UncarriableReport};
use metsuke_wire::envelope::{self, Envelope, Payload, PayloadLine, Scrape, SigningKey};

pub struct Delivery {
    /// Also where the pool and agent a batch names come from: the spool stamped
    /// every line it holds with them, so taking the header's identity from
    /// anywhere else would let the two disagree.
    spool: Spool,
    key: SigningKey,
    /// zstd level passed to `seal` (0 = zstd's default).
    compression_level: i32,
    /// Pre-compression ceiling on one envelope: its header frame plus its
    /// payload lines. The server's own ceiling is `[ingest].max_body_bytes`, on
    /// the compressed bytes it receives. It is still the agent's own number:
    /// nothing in the wire contract lets it discover the server's, so a batch
    /// over that is rejected at upload and stays spooled.
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
    Scrapes(Vec<i64>),
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
        compression_level: i32,
        batch_max_bytes: u64,
    ) -> Self {
        Delivery {
            spool,
            key,
            compression_level,
            batch_max_bytes,
        }
    }

    /// Append a scrape to the spool. Returns how many rows the spool's cap
    /// dropped to make room.
    pub fn push(&mut self, scrape: &Scrape) -> Result<u64, DeliveryError> {
        Ok(self.spool.push(scrape)?)
    }

    /// Seal one batch of outstanding scrapes, drawing (and persisting) the
    /// next counter. `None` when nothing is spooled. Rows stay spooled
    /// until `ack`; a retry after a failed PUT simply takes a fresh batch.
    pub fn take_batch(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        let rows = self
            .spool
            .outstanding(self.row_budget(now, Payload::scrapes(vec![]))?)?;
        self.batch(now, rows, Payload::scrapes, BatchRows::Scrapes)
    }

    /// The same for outstanding trace lines.
    pub fn take_line_batch(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        let rows = self
            .spool
            .outstanding_lines(self.row_budget(now, Payload::trace_lines(vec![]))?)?;
        self.batch(now, rows, Payload::trace_lines, BatchRows::Lines)
    }

    /// Seal what a stream offered, as the schema that stream holds. The rows
    /// arrive as the lines they will be on the wire, so the two streams differ
    /// only in which schema the payload declares and which table an ACK deletes
    /// from.
    fn batch(
        &mut self,
        now: OffsetDateTime,
        rows: Vec<SpooledRow>,
        payload: fn(Vec<PayloadLine>) -> Payload,
        acks: fn(Vec<i64>) -> BatchRows,
    ) -> Result<Option<SealedBatch>, DeliveryError> {
        if rows.is_empty() {
            return Ok(None);
        }
        let (ids, lines) = rows.into_iter().map(|row| (row.id, row.line)).unzip();
        self.seal(now, payload(lines), acks(ids)).map(Some)
    }

    /// The budget the spool takes rows against (`spool::RowBudget`), measured by
    /// building the header rather than by a second account of its fields.
    /// `u64::MAX` is the widest counter this agent can ever draw, so the reserve
    /// never comes up short of the counter the batch is actually stamped with.
    ///
    /// A budget the framing already exhausts is an error, not a zero: every
    /// row is over a zero budget, so offering one would have the spool drop the
    /// head row a tick for as long as the config stands
    /// (`spool::outstanding_rows`).
    fn row_budget(&self, now: OffsetDateTime, empty: Payload) -> Result<RowBudget, DeliveryError> {
        let envelope = self.envelope(u64::MAX, now, empty);
        let header = envelope::header_json(&envelope)?;
        let framing = (envelope::HEADER_OFFSET + header.len()) as u64;
        match self.batch_max_bytes.checked_sub(framing) {
            Some(max_bytes) if max_bytes > 0 => Ok(RowBudget { max_bytes }),
            _ => Err(DeliveryError::BudgetBelowFraming {
                batch_max_bytes: self.batch_max_bytes,
                framing,
            }),
        }
    }

    fn envelope(&self, counter: u64, now: OffsetDateTime, payload: Payload) -> Envelope {
        Envelope::new(
            self.spool.provenance().clone(),
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

    /// What the last batches dropped rather than sealed
    /// (`spool::Spool::take_uncarriable_report`).
    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        self.spool.take_uncarriable_report()
    }

    /// The server ACK'd this batch: delete exactly the rows it sealed.
    pub fn ack(&mut self, batch: SealedBatch) -> Result<(), DeliveryError> {
        match batch.rows {
            BatchRows::Scrapes(ids) => self.spool.ack(&ids)?,
            BatchRows::Lines(ids) => self.spool.ack_lines(&ids)?,
        }
        Ok(())
    }
}
