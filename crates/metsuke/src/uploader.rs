//! One submission upload: POST the sealed bytes with the ADR-0001 header
//! contract and classify the answer. Never touches the spool: the caller
//! acks on `Acked` and leaves rows in place otherwise (ADR 0004).

use std::num::NonZeroUsize;
use std::time::Duration;

use crate::delivery::SealedSubmission;
use crate::endpoint::UploadUrl;
use metsuke_wire::envelope::{Ack, HEADER_POOL, PoolId};
use metsuke_wire::http;

pub struct UploadConfig {
    pub upload_url: UploadUrl,
    /// Whole-request deadline, as bounded by `metsuke_wire::http::agent`.
    pub timeout: Duration,
    /// What one tick may send (`agent::Agent::upload_once`). Read there rather
    /// than here: one POST is one POST whatever the tick's allowance is.
    pub max_submissions: NonZeroUsize,
}

/// What one upload attempt means for the spool and the schedule.
#[derive(Debug)]
pub enum UploadOutcome {
    /// The server stored the submission: ack the rows.
    Acked(Ack),
    /// Transport failure, or any status `metsuke_wire::http::classify` reads
    /// as retryable: the server may recover on its own; retry next interval.
    Retryable(String),
    /// The server refused with a reason an operator must act on; back off.
    Rejected { status: u16, reason: String },
}

/// POST one sealed submission. Infallible by design: every failure mode is a
/// scheduling decision, not an error path.
pub fn upload(
    config: &UploadConfig,
    pool_id: PoolId,
    submission: &SealedSubmission,
) -> UploadOutcome {
    let [vkey, signature] = submission.attestation.headers();
    let response = http::agent(config.timeout)
        .post(config.upload_url.as_str())
        .header(vkey.0, vkey.1)
        .header(signature.0, signature.1)
        // Sent whichever key signed: under a Leios key it is the only thing
        // that says which pool this is, and under a cold key it is the
        // derivation stated twice, which the server checks (ADR 0011).
        .header(HEADER_POOL, pool_id.to_bech32())
        .header("content-encoding", "zstd")
        .content_type("application/json")
        .send(&submission.wire_bytes[..]);
    let mut response = match response {
        Ok(response) => response,
        Err(error) => return UploadOutcome::Retryable(error.to_string()),
    };
    match http::classify(&mut response) {
        Ok(()) => match response
            .body_mut()
            .read_to_string()
            .map_err(|error| error.to_string())
            .and_then(|body| serde_json::from_str::<Ack>(&body).map_err(|error| error.to_string()))
        {
            Ok(ack) => UploadOutcome::Acked(ack),
            // A 2xx without a parseable ack: the PUT may not have happened;
            // keep the rows and retry.
            Err(error) => UploadOutcome::Retryable(format!("unreadable ack: {error}")),
        },
        Err(refusal) if refusal.retryable => UploadOutcome::Retryable(format!(
            "server answered {}: {}",
            refusal.status, refusal.reason
        )),
        Err(refusal) => UploadOutcome::Rejected {
            status: refusal.status,
            reason: refusal.reason,
        },
    }
}

/// True when the ack's `latest_version` is newer than this build, the
/// ADR-0006 update nudge. Segments compare numerically; a version that
/// doesn't parse cannot claim to be newer.
pub fn newer_version_available(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(current), Some(latest)) => latest > current,
        _ => false,
    }
}

fn parse_version(version: &str) -> Option<Vec<u64>> {
    version
        .split('.')
        .map(|segment| segment.parse().ok())
        .collect()
}
