//! The two endpoints the agent may talk to. They share this module because
//! they share the loopback test.

use serde::Deserialize;

/// The node's PrometheusSimple endpoint, loopback only (ADR 0007).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct MetricsUrl(String);

/// An endpoint the signed body may go to. `TryFrom` is the only way in, so
/// no `UploadConfig` can name a plaintext endpoint out on the network.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "String")]
pub struct UploadUrl(String);

#[derive(Debug, thiserror::Error)]
pub enum MetricsUrlError {
    #[error("metrics_url {0}")]
    NotAbsolute(#[source] NotAbsolute),
    #[error("metrics_url must be http or https to a loopback address, got {0:?}")]
    NotLoopback(String),
}

#[derive(Debug, thiserror::Error)]
pub enum UploadUrlError {
    #[error("upload_url {0}")]
    NotAbsolute(#[source] NotAbsolute),
    /// Plaintext to loopback cannot be intercepted, because it never leaves
    /// the host. Anywhere else it exposes the whole batch.
    #[error("upload_url must be https, or http to a loopback address, got {0:?}")]
    Plaintext(String),
}

/// Why `text` names nowhere at all, as opposed to somewhere a given endpoint
/// refuses to talk to. Each endpoint's error prefixes its own field name.
#[derive(Debug, thiserror::Error)]
pub enum NotAbsolute {
    #[error("does not parse as a URL: {value:?}")]
    Malformed {
        value: String,
        #[source]
        source: ureq::http::uri::InvalidUri,
    },
    #[error("must be an absolute URL with a host, got {0:?}")]
    NoHost(String),
}

impl TryFrom<String> for MetricsUrl {
    type Error = MetricsUrlError;

    fn try_from(text: String) -> Result<MetricsUrl, MetricsUrlError> {
        let (scheme, host) = absolute_parts(&text).map_err(MetricsUrlError::NotAbsolute)?;
        match scheme.as_str() {
            "http" | "https" if is_loopback(&host) => Ok(MetricsUrl(text)),
            _ => Err(MetricsUrlError::NotLoopback(text)),
        }
    }
}

impl TryFrom<String> for UploadUrl {
    type Error = UploadUrlError;

    fn try_from(text: String) -> Result<UploadUrl, UploadUrlError> {
        let (scheme, host) = absolute_parts(&text).map_err(UploadUrlError::NotAbsolute)?;
        match scheme.as_str() {
            "https" => Ok(UploadUrl(text)),
            "http" if is_loopback(&host) => Ok(UploadUrl(text)),
            _ => Err(UploadUrlError::Plaintext(text)),
        }
    }
}

/// The scheme and host of `text`, which an endpoint needs both of to name
/// somewhere.
fn absolute_parts(text: &str) -> Result<(String, String), NotAbsolute> {
    let uri = text
        .parse::<ureq::http::Uri>()
        .map_err(|source| NotAbsolute::Malformed {
            value: text.to_owned(),
            source,
        })?;
    match (uri.scheme_str(), uri.host()) {
        (Some(scheme), Some(host)) => Ok((scheme.to_owned(), host.to_owned())),
        _ => Err(NotAbsolute::NoHost(text.to_owned())),
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

impl MetricsUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl UploadUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MetricsUrl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}
