//! One batch upload: POST the sealed bytes with the ADR-0001 header
//! contract and classify the answer. Never touches the spool — the caller
//! acks on `Acked` and leaves rows in place otherwise (ADR 0004).

use std::time::Duration;

use crate::delivery::SealedBatch;
use crate::envelope::{Ack, HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, VerifyingKey};

pub struct UploadConfig {
    pub upload_url: String,
    pub pool_id: PoolId,
    /// Whole-request deadline: connect, send, and response read together.
    pub timeout: Duration,
}

/// What one upload attempt means for the spool and the schedule.
#[derive(Debug)]
pub enum UploadOutcome {
    /// The server stored the batch: ack the rows.
    Acked(Ack),
    /// Transport failure or 5xx: the server may recover on its own; retry
    /// next interval.
    Retryable(String),
    /// 4xx: the server named a reason an operator must act on; back off.
    Rejected { status: u16, reason: String },
}

/// POST one sealed batch. Infallible by design: every failure mode is a
/// scheduling decision, not an error path.
pub fn upload(config: &UploadConfig, vkey: &VerifyingKey, batch: &SealedBatch) -> UploadOutcome {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(config.timeout))
        // Status handling is ours: 4xx and 5xx are outcomes, not errors.
        .http_status_as_error(false)
        .build()
        .into();
    let response = agent
        .post(&config.upload_url)
        .header(HEADER_POOL_ID, config.pool_id.to_bech32())
        .header(HEADER_VKEY, hex(vkey.as_bytes()))
        .header(HEADER_SIGNATURE, hex(&batch.signature.to_bytes()))
        .header("content-encoding", "zstd")
        .content_type("application/json")
        .send(&batch.wire_bytes[..]);
    let mut response = match response {
        Ok(response) => response,
        Err(error) => return UploadOutcome::Retryable(error.to_string()),
    };
    let status = response.status().as_u16();
    match status {
        200..=299 => match response
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
        400..=499 => UploadOutcome::Rejected {
            status,
            reason: response
                .body_mut()
                .read_to_string()
                .unwrap_or_else(|error| format!("unreadable reason: {error}")),
        },
        _ => UploadOutcome::Retryable(format!("server answered {status}")),
    }
}

/// True when the ack's `latest_version` is newer than this build — the
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
