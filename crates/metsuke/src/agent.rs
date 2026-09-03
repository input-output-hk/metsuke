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
use metsuke_wire::envelope::PoolId;

pub struct Agent {
    scraper: ScraperConfig,
    delivery: Delivery,
    upload: UploadConfig,
    /// Which pool every submission names. A Leios key derives none, so this
    /// is what the header carries and what the server looks the key up under
    /// (ADR 0011).
    pool_id: PoolId,
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
    /// `delivery::SealedSubmission::payload_digest`.
    pub payload_digest: String,
}

impl Agent {
    pub fn new(
        scraper: ScraperConfig,
        delivery: Delivery,
        upload: UploadConfig,
        pool_id: PoolId,
    ) -> Self {
        Agent {
            scraper,
            delivery,
            upload,
            pool_id,
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

    /// One upload tick: scrapes, then trace lines, each stream drained until it
    /// is empty or the tick's allowance is spent. Every submission is sealed,
    /// POSTed and acked only on `Acked`.
    ///
    /// Draining rather than sending one of each is what keeps a spool from
    /// filling: a node emits more between ticks than one submission carries, so
    /// a tick that sent one left the difference behind every hour until the cap
    /// discarded it.
    ///
    /// Every attempt comes back, not just the last, because a tick that sent
    /// several is several lines in the journal. The caller schedules on the
    /// last, and a submission the server did not take ends the tick, because
    /// pressing on would ignore the answer.
    pub fn upload_once(&mut self) -> Result<Vec<Uploaded>, UploadError> {
        type Take =
            fn(&mut Delivery, OffsetDateTime) -> Result<Option<SealedSubmission>, DeliveryError>;
        let streams: [Take; 2] = [Delivery::take_submission, Delivery::take_line_submission];

        let now = OffsetDateTime::now_utc();
        let allowance = self.upload.max_submissions.get();
        let mut sent = Vec::new();
        for take in streams {
            while sent.len() < allowance {
                let taken = take(&mut self.delivery, now).map_err(UploadError::NotAttempted)?;
                // What a tick drains is the backlog it found. A batch that
                // took everything spooled says there is none left, and a
                // stream filling faster than a round trip would otherwise be
                // chased to the allowance, spending a request, a counter and
                // an object on each handful that arrived mid-tick.
                let more = taken.as_ref().is_some_and(SealedSubmission::more_waits);
                let Some(one) = self.send(taken)? else {
                    break;
                };
                let accepted = matches!(one.outcome, UploadOutcome::Acked(_));
                sent.push(one);
                // A refusal ends the tick, because pressing on would ignore
                // the answer; a drained stream ends only this one, because
                // the other still has its own backlog to send.
                if !accepted {
                    return Ok(sent);
                }
                if !more {
                    break;
                }
            }
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
            outcome: upload(&self.upload, self.pool_id, &submission),
            counter: submission.counter,
            lines: submission.lines(),
            carried: submission.carried(),
            bytes: submission.wire_bytes.len(),
            payload_digest: submission.payload_digest.clone(),
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
