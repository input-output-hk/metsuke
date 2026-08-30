//! The agent loop body: `scrape_once` and `upload_once` are the two ticks
//! the binary schedules. Owning delivery and upload together makes "ack
//! exactly the rows that were sealed, and only on `Acked`" (ADR 0004) the
//! only expressible call sequence; when to tick and what to log stay with
//! the caller.

use time::OffsetDateTime;

use crate::delivery::{Delivery, DeliveryError, SealedSubmission};
use crate::scrape::Refused;
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
/// accepted the submission: an ack that fails locally after acceptance means
/// the same rows will be resubmitted, not that the upload never happened.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("upload not attempted: {0}")]
    NotAttempted(#[source] DeliveryError),
    #[error(
        "submission accepted by the server but not acked locally \
         (rows will be resubmitted): {0}"
    )]
    AckAfterAccept(#[source] DeliveryError),
}

/// What one scrape tick has for the journal. The row goes straight to the
/// spool, so what an operator would want said about it comes back here rather
/// than being read off the row afterwards.
#[derive(Debug, Default)]
pub struct ScrapeNews {
    /// The detail of the failure the row shipped, when the scrape failed.
    pub failed: Option<String>,
    /// The body's lines that reached no metric.
    pub refused: Vec<Refused>,
}

/// What one upload tick has for the journal: the server's answer, and which
/// submission it answered. The tick consumes the sealed submission, so what a
/// log line needs off it comes out here.
#[derive(Debug)]
pub struct Uploaded {
    pub outcome: UploadOutcome,
    /// The counter its header carries.
    pub counter: u64,
    /// How many lines it carried, and what to call them
    /// (`delivery::SealedSubmission::carried`).
    pub lines: usize,
    pub carried: &'static str,
    /// The sealed bytes, as sent.
    pub bytes: usize,
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
    pub fn scrape_once(&mut self) -> Result<ScrapeNews, DeliveryError> {
        let (row, refused) = scrape_once(&self.scraper);
        let failed = row.failure.as_ref().map(|failure| failure.detail.clone());
        self.dropped_since_report += self.delivery.push(&row)?;
        Ok(ScrapeNews { failed, refused })
    }

    /// One upload tick: a submission of scrapes, then one of trace lines,
    /// each sealed, POSTed and acked only on `Acked`. Empty when both streams
    /// are. Every attempt comes back, not just the last: a tick that sent two
    /// submissions is two lines in the journal, and reporting only the second
    /// would leave an agent's scrapes looking unsent while the server was
    /// storing them. The caller schedules on the last, and a submission that
    /// was not accepted ends the tick, because backing off on the scrapes and
    /// then pressing on with the lines would ignore the answer.
    pub fn upload_once(&mut self) -> Result<Vec<Uploaded>, UploadError> {
        let now = OffsetDateTime::now_utc();
        let taken = self
            .delivery
            .take_submission(now)
            .map_err(UploadError::NotAttempted)?;
        let mut sent = Vec::from_iter(self.send(taken)?);
        // Vacuously true when the scrape stream was empty, which is the tick
        // that has only trace lines to offer.
        if sent
            .iter()
            .all(|one| matches!(one.outcome, UploadOutcome::Acked(_)))
        {
            let taken = self
                .delivery
                .take_line_submission(now)
                .map_err(UploadError::NotAttempted)?;
            sent.extend(self.send(taken)?);
        }
        Ok(sent)
    }

    /// POST one submission if there is one, acking its rows only on `Acked`.
    fn send(
        &mut self,
        submission: Option<SealedSubmission>,
    ) -> Result<Option<Uploaded>, UploadError> {
        let Some(submission) = submission else {
            return Ok(None);
        };
        let sent = Uploaded {
            outcome: upload(&self.upload, &self.vkey, &submission),
            counter: submission.counter,
            lines: submission.lines(),
            carried: submission.carried(),
            bytes: submission.wire_bytes.len(),
        };
        if matches!(sent.outcome, UploadOutcome::Acked(_)) {
            self.delivery
                .ack(submission)
                .map_err(UploadError::AckAfterAccept)?;
        }
        Ok(Some(sent))
    }

    /// How many rows the spool's cap dropped since this was last asked, and
    /// zero from here until the next drop.
    pub fn take_dropped_report(&mut self) -> u64 {
        std::mem::take(&mut self.dropped_since_report)
    }

    /// What taking a submission dropped for being uncarriable
    /// (`delivery::Delivery::take_uncarriable_report`). A separate report from
    /// `take_dropped_report`: neither remedy is a faster upload.
    pub fn take_uncarriable_report(&mut self) -> UncarriableReport {
        self.delivery.take_uncarriable_report()
    }
}
