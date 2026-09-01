//! Submission delivery: the only path from spooled rows to a sealed upload.
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
use metsuke_wire::envelope::{self, Envelope, Payload, PayloadLine, Scrape, SubmissionKey};

pub struct Delivery {
    /// Also where the pool and agent a submission names come from: the spool stamped
    /// every line it holds with them, so taking the header's identity from
    /// anywhere else would let the two disagree.
    spool: Spool,
    key: SubmissionKey,
    /// zstd level passed to `seal` (0 = zstd's default).
    compression_level: i32,
    /// Pre-compression ceiling on one envelope: its header frame plus its
    /// payload lines. The server's own ceiling is `[ingest].max_body_bytes`, on
    /// the compressed bytes it receives. It is still the agent's own number:
    /// nothing in the wire contract lets it discover the server's, so a
    /// submission over that is rejected at upload and stays spooled.
    batch_max_bytes: u64,
}

/// One sealed upload: the bytes to PUT and what attests them.
/// The rows it covers stay private, so the only rows an `ack` can delete are
/// the ones sealed into this submission, and consuming it prevents a double
/// ack.
pub struct SealedSubmission {
    pub wire_bytes: Vec<u8>,
    pub attestation: envelope::Attestation,
    /// What the header stamped this with, so a journal line and an archived
    /// object can be matched by it.
    pub counter: u64,
    /// What its rows are rather than what this attempt at them is
    /// (`envelope::payload_digest`).
    pub payload_digest: String,
    rows: SubmissionRows,
}

impl SealedSubmission {
    /// How many lines it carries.
    pub fn lines(&self) -> usize {
        match &self.rows {
            SubmissionRows::Scrapes(ids) | SubmissionRows::Lines(ids) => ids.len(),
        }
    }

    /// What to call those lines, so an operator running `[log]` can tell the
    /// two submissions of one tick apart in the journal. Agrees with the count
    /// it is printed beside.
    pub fn carried(&self) -> &'static str {
        match (&self.rows, self.lines()) {
            (SubmissionRows::Scrapes(_), 1) => "scrape",
            (SubmissionRows::Scrapes(_), _) => "scrapes",
            (SubmissionRows::Lines(_), 1) => "trace line",
            (SubmissionRows::Lines(_), _) => "trace lines",
        }
    }
}

/// Which stream's rows a submission drew from, so `ack` deletes from the table
/// it sealed rather than from whichever one the caller remembers.
enum SubmissionRows {
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
        key: SubmissionKey,
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

    /// Seal one submission of outstanding scrapes, drawing (and persisting) the
    /// next counter. `None` when nothing is spooled. Rows stay spooled
    /// until `ack`; a retry after a failed PUT simply takes a fresh submission.
    pub fn take_submission(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedSubmission>, DeliveryError> {
        let rows = self
            .spool
            .outstanding(self.row_budget(now, Payload::scrapes(vec![]))?)?;
        self.seal_rows(now, rows, Payload::scrapes, SubmissionRows::Scrapes)
    }

    /// The same for outstanding trace lines.
    pub fn take_line_submission(
        &mut self,
        now: OffsetDateTime,
    ) -> Result<Option<SealedSubmission>, DeliveryError> {
        let rows = self
            .spool
            .outstanding_lines(self.row_budget(now, Payload::trace_lines(vec![]))?)?;
        self.seal_rows(now, rows, Payload::trace_lines, SubmissionRows::Lines)
    }

    /// Seal what a stream offered, as the schema that stream holds. The rows
    /// arrive as the lines they will be on the wire, so the two streams differ
    /// only in which schema the payload declares and which table an ACK deletes
    /// from.
    fn seal_rows(
        &mut self,
        now: OffsetDateTime,
        rows: Vec<SpooledRow>,
        payload: fn(Vec<PayloadLine>) -> Payload,
        acks: fn(Vec<i64>) -> SubmissionRows,
    ) -> Result<Option<SealedSubmission>, DeliveryError> {
        if rows.is_empty() {
            return Ok(None);
        }
        let (ids, lines) = rows.into_iter().map(|row| (row.id, row.line)).unzip();
        self.seal(now, payload(lines), acks(ids)).map(Some)
    }

    /// The budget the spool takes rows against (`spool::RowBudget`), measured by
    /// building the header rather than by a second account of its fields.
    /// `u64::MAX` is the widest counter this agent can ever draw, so the reserve
    /// never comes up short of the counter the submission is actually stamped with.
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
        rows: SubmissionRows,
    ) -> Result<SealedSubmission, DeliveryError> {
        let counter = self.spool.next_counter()?;
        let envelope = self.envelope(counter, now, payload);
        let payload_digest = envelope::payload_digest(&envelope);
        let (wire_bytes, attestation) =
            envelope::seal(&self.key, &envelope, self.compression_level)?;
        Ok(SealedSubmission {
            wire_bytes,
            attestation,
            counter,
            payload_digest,
            rows,
        })
    }

    /// What the last submissions dropped rather than sealed
    /// (`spool::Spool::take_uncarriable_report`).
    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        self.spool.take_uncarriable_report()
    }

    /// The server ACK'd this submission: delete exactly the rows it sealed.
    pub fn ack(&mut self, submission: SealedSubmission) -> Result<(), DeliveryError> {
        match submission.rows {
            SubmissionRows::Scrapes(ids) => self.spool.ack(&ids)?,
            SubmissionRows::Lines(ids) => self.spool.ack_lines(&ids)?,
        }
        Ok(())
    }
}
