//! The HTTP client both binaries build, and the one reading of a non-2xx
//! answer they both act on. It lives here because a status split that
//! disagreed between them would make a retry loop mean one thing per call
//! site.

use std::time::Duration;

/// An agent bounded whole-request by `timeout` — connect, send and read
/// together — which hands a status back as an answer rather than an error,
/// because a refusal is a reason to report and `classify` reads it.
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// A non-2xx answer, its body read as the reason. `retryable` separates an
/// endpoint that may come back from a refusal that will answer the same way
/// twice.
#[derive(Debug)]
pub struct Refusal {
    pub status: u16,
    pub reason: String,
    pub retryable: bool,
}

/// Split a 2xx from everything else, leaving a success's body unread for the
/// caller. Only a 4xx is terminal: credentials, policy or a clock, which
/// retrying delays the reason for without changing it. Everything else may
/// answer differently next time.
pub fn classify(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), Refusal> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(Refusal {
        status,
        reason: response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|error| format!("unreadable reason: {error}")),
        retryable: !(400..500).contains(&status),
    })
}
