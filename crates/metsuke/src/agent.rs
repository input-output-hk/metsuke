//! The agent loop body: `scrape_once` and `upload_once` are the two ticks
//! the binary schedules. Owning delivery and upload together makes "ack
//! exactly the rows that were sealed, and only on `Acked`" (ADR 0004) the
//! only expressible call sequence; when to tick and what to log stay with
//! the caller.

use time::OffsetDateTime;

use crate::delivery::{Delivery, DeliveryError, SealedBatch};
use crate::scraper::{ScraperConfig, scrape_once};
use crate::spool::UncarriableReport;
use crate::uploader::{UploadConfig, UploadOutcome, upload};
use metsuke_wire::envelope::VerifyingKey;

pub struct Agent {
    scraper: ScraperConfig,
    delivery: Delivery,
    upload: UploadConfig,
    vkey: VerifyingKey,
    /// Rows the spool's cap dropped since the last report. Accumulated rather
    /// than logged per row: under sustained overload the drop rate is the
    /// spool's write rate, and one line each would be the loudest thing in the
    /// journal.
    dropped_since_report: u64,
}

/// Upload-tick failures, split so the log can say whether the server
/// accepted the batch: an ack that fails locally after acceptance means the
/// same rows will be resubmitted, not that the upload never happened.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("upload not attempted: {0}")]
    NotAttempted(#[source] DeliveryError),
    #[error(
        "batch accepted by the server but not acked locally \
         (rows will be resubmitted): {0}"
    )]
    AckAfterAccept(#[source] DeliveryError),
}

impl Agent {
    pub fn new(
        scraper: ScraperConfig,
        delivery: Delivery,
        upload: UploadConfig,
        vkey: VerifyingKey,
    ) -> Self {
        Agent {
            scraper,
            delivery,
            upload,
            vkey,
            dropped_since_report: 0,
        }
    }

    /// One scrape tick: read, probe, spool.
    pub fn scrape_once(&mut self) -> Result<(), DeliveryError> {
        self.dropped_since_report += self.delivery.push(&scrape_once(&self.scraper))?;
        Ok(())
    }

    /// One upload tick: a batch of scrapes, then a batch of trace lines,
    /// each sealed, POSTed and acked only on `Acked`. `None` when both
    /// streams are empty. The last outcome is what the caller schedules on,
    /// and a batch that was not accepted ends the tick. Backing off on the
    /// scrapes and then pressing on with the lines would ignore the answer.
    pub fn upload_once(&mut self) -> Result<Option<UploadOutcome>, UploadError> {
        let now = OffsetDateTime::now_utc();
        let taken = self
            .delivery
            .take_batch(now)
            .map_err(UploadError::NotAttempted)?;
        let scrapes = self.send(taken)?;
        if matches!(scrapes, None | Some(UploadOutcome::Acked(_))) {
            let taken = self
                .delivery
                .take_line_batch(now)
                .map_err(UploadError::NotAttempted)?;
            if let Some(lines) = self.send(taken)? {
                return Ok(Some(lines));
            }
        }
        Ok(scrapes)
    }

    /// POST one batch if there is one, acking its rows only on `Acked`.
    fn send(&mut self, batch: Option<SealedBatch>) -> Result<Option<UploadOutcome>, UploadError> {
        let Some(batch) = batch else {
            return Ok(None);
        };
        let outcome = upload(&self.upload, &self.vkey, &batch);
        if matches!(outcome, UploadOutcome::Acked(_)) {
            self.delivery
                .ack(batch)
                .map_err(UploadError::AckAfterAccept)?;
        }
        Ok(Some(outcome))
    }

    /// How many rows the spool's cap dropped since this was last asked, and
    /// zero from here until the next drop.
    pub fn take_dropped_report(&mut self) -> u64 {
        std::mem::take(&mut self.dropped_since_report)
    }

    /// What taking a batch dropped for being uncarriable
    /// (`delivery::Delivery::take_uncarriable_report`). A separate report from
    /// `take_dropped_report`: neither remedy is a faster upload.
    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        self.delivery.take_uncarriable_report()
    }
}
