//! One batch upload: POST the sealed bytes with the ADR-0001 header
//! contract and classify the answer. Never touches the spool — the caller
//! acks on `Acked` and leaves rows in place otherwise (ADR 0004).

use std::time::Duration;

use serde::Deserialize;

use crate::delivery::SealedBatch;
use metsuke_wire::envelope::{
    Ack, HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, VerifyingKey,
};
use metsuke_wire::{hex, http};

/// An endpoint the signed body may go to. `TryFrom` is the only way in, so
/// no `UploadConfig` can name a plaintext endpoint out on the network.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct UploadUrl(String);

#[derive(Debug, thiserror::Error)]
pub enum UploadUrlError {
    #[error("upload_url does not parse as a URL: {value:?}")]
    Malformed {
        value: String,
        #[source]
        source: ureq::http::uri::InvalidUri,
    },
    #[error("upload_url must be an absolute URL with a host, got {0:?}")]
    NotAbsolute(String),
    /// Plaintext to loopback cannot be intercepted, because it never leaves
    /// the host. Anywhere else it exposes the whole batch.
    #[error("upload_url must be https, or http to a loopback address, got {0:?}")]
    Plaintext(String),
}

impl TryFrom<String> for UploadUrl {
    type Error = UploadUrlError;

    fn try_from(text: String) -> Result<UploadUrl, UploadUrlError> {
        let uri = text
            .parse::<ureq::http::Uri>()
            .map_err(|source| UploadUrlError::Malformed {
                value: text.clone(),
                source,
            })?;
        let (Some(scheme), Some(host)) = (uri.scheme_str(), uri.host()) else {
            return Err(UploadUrlError::NotAbsolute(text));
        };
        match scheme {
            "https" => Ok(UploadUrl(text)),
            "http" if is_loopback(host) => Ok(UploadUrl(text)),
            _ => Err(UploadUrlError::Plaintext(text)),
        }
    }
}

/// An IP literal that routes back to this host. A name is not enough: what
/// `localhost` resolves to is a file the agent does not read.
fn is_loopback(host: &str) -> bool {
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

impl UploadUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub struct UploadConfig {
    pub upload_url: UploadUrl,
    pub pool_id: PoolId,
    /// Whole-request deadline, as bounded by `metsuke_wire::http::agent`.
    pub timeout: Duration,
}

/// What one upload attempt means for the spool and the schedule.
#[derive(Debug)]
pub enum UploadOutcome {
    /// The server stored the batch: ack the rows.
    Acked(Ack),
    /// Transport failure, or any status `metsuke_wire::http::classify` reads
    /// as retryable: the server may recover on its own; retry next interval.
    Retryable(String),
    /// The server refused with a reason an operator must act on; back off.
    Rejected { status: u16, reason: String },
}

/// POST one sealed batch. Infallible by design: every failure mode is a
/// scheduling decision, not an error path.
pub fn upload(config: &UploadConfig, vkey: &VerifyingKey, batch: &SealedBatch) -> UploadOutcome {
    let response = http::agent(config.timeout)
        .post(config.upload_url.as_str())
        .header(HEADER_POOL_ID, config.pool_id.to_bech32())
        .header(HEADER_VKEY, hex::encode(vkey.as_bytes()))
        .header(HEADER_SIGNATURE, hex::encode(&batch.signature.to_bytes()))
        .header("content-encoding", "zstd")
        .content_type("application/json")
        .send(&batch.wire_bytes[..]);
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
