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

/// What one upload tick has for the journal: every submission it sent, and the
/// failure that ended it if one did.
///
/// Both, and not one or the other. A tick that sent two and then could not ack
/// the third put three objects in the archive, and returning only the failure
/// left all three unnamed while the caller reported a tick that sent nothing.
/// Which submission a line is about is what ties it to an archived object, so
/// the ones that landed are the ones an operator most needs named.
#[derive(Debug)]
pub struct UploadTick {
    pub sent: Vec<Uploaded>,
    /// `None` where the tick ran out of submissions or the server ended it.
    /// A tick that failed always sent an accepted submission last, because a
    /// server that did not take one ends the tick before the next is read.
    pub failed: Option<UploadError>,
}

impl UploadTick {
    fn done(sent: Vec<Uploaded>) -> UploadTick {
        UploadTick { sent, failed: None }
    }

    fn ended(sent: Vec<Uploaded>, failed: UploadError) -> UploadTick {
        UploadTick {
            sent,
            failed: Some(failed),
        }
    }
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
    ///
    /// A failure ends the tick without discarding what it sent
    /// (`UploadTick`), so it is not a `Result`: every submission this reached
    /// the server with is an object in the archive whether or not the tick
    /// finished.
    pub fn upload_once(&mut self) -> UploadTick {
        type Take =
            fn(&mut Delivery, OffsetDateTime) -> Result<Option<SealedSubmission>, DeliveryError>;
        let streams: [Take; 2] = [Delivery::take_submission, Delivery::take_line_submission];

        let now = OffsetDateTime::now_utc();
        let allowance = self.upload.max_submissions.get();
        let mut sent = Vec::new();
        for take in streams {
            while sent.len() < allowance {
                let taken = match take(&mut self.delivery, now) {
                    Ok(taken) => taken,
                    Err(error) => {
                        return UploadTick::ended(sent, UploadError::NotAttempted(error));
                    }
                };
                // Whether a row waited above the highest id this batch took,
                // asked before the POST so what lands during one is invisible
                // to the decision to continue. Mid-tick arrivals are carried
                // by the next batch but cannot keep the loop alive on their
                // own: the drain ends at the first batch nothing waited
                // behind, and what landed during that round trip waits for
                // the next tick. A stream filling faster than a batch per
                // round trip drains to the allowance: a backlog, not a chase.
                let more = taken.as_ref().is_some_and(SealedSubmission::more_waits);
                let Some(submission) = taken else {
                    break;
                };
                // Recorded before the ack, which is the only thing after this
                // that can fail, and which failing does not unsend it.
                let record = self.posted(&submission);
                let accepted = matches!(record.outcome, UploadOutcome::Acked(_));
                sent.push(record);
                // A refusal ends the tick, because pressing on would ignore
                // the answer; a drained stream ends only this one, because
                // the other still has its own backlog to send.
                if !accepted {
                    return UploadTick::done(sent);
                }
                if let Err(error) = self.delivery.ack(submission) {
                    return UploadTick::ended(sent, UploadError::AckAfterAccept(error));
                }
                if !more {
                    break;
                }
            }
        }
        UploadTick::done(sent)
    }

    /// POST one submission and record what the server said. Everything a log
    /// line needs comes off the submission here, because acking it consumes
    /// it.
    fn posted(&self, submission: &SealedSubmission) -> Uploaded {
        Uploaded {
            outcome: upload(&self.upload, self.pool_id, submission),
            counter: submission.counter,
            lines: submission.lines(),
            carried: submission.carried(),
            bytes: submission.wire_bytes.len(),
            payload_digest: submission.payload_digest.clone(),
        }
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
