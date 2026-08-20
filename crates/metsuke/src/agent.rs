//! The agent loop body: `sample_once` and `upload_once` are the two ticks
//! the binary schedules. Owning delivery and upload together makes "ack
//! exactly the rows that were sealed, and only on `Acked`" (ADR 0004) the
//! only expressible call sequence; when to tick and what to log stay with
//! the caller.

use time::OffsetDateTime;

use crate::delivery::{Delivery, DeliveryError};
use crate::envelope::VerifyingKey;
use crate::sampler::{SamplerConfig, sample};
use crate::uploader::{UploadConfig, UploadOutcome, upload};

pub struct Agent {
    sampler: SamplerConfig,
    delivery: Delivery,
    upload: UploadConfig,
    vkey: VerifyingKey,
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
        sampler: SamplerConfig,
        delivery: Delivery,
        upload: UploadConfig,
        vkey: VerifyingKey,
    ) -> Self {
        Agent {
            sampler,
            delivery,
            upload,
            vkey,
        }
    }

    /// One sample tick: scrape, probe, spool.
    pub fn sample_once(&mut self) -> Result<(), DeliveryError> {
        self.delivery.push(&sample(&self.sampler))
    }

    /// One upload tick: seal everything outstanding, POST it, and ack the
    /// sealed rows only on `Acked`. `None` when the spool is empty.
    pub fn upload_once(&mut self) -> Result<Option<UploadOutcome>, UploadError> {
        let Some(batch) = self
            .delivery
            .take_batch(OffsetDateTime::now_utc())
            .map_err(UploadError::NotAttempted)?
        else {
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
}
