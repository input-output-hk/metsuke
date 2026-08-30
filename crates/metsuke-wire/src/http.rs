//! The HTTP client every binary builds, the one reading of a non-2xx answer
//! they all act on, and the archive-pull contract the server answers and the
//! fetch tool reads. A status split or a route that disagreed between them
//! would make a retry loop or a sync mean one thing per call site.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The two routes an archive pull uses: one page of the listing, and one
/// object's bytes.
pub const SUBMISSIONS_PATH: &str = "/v1/submissions";
pub const OBJECT_PATH: &str = "/v1/object";

/// The listing's two filters, as query fields: the literal head of an object
/// key, and the last key the client already has.
pub const PREFIX_FIELD: &str = "prefix";
pub const AFTER_FIELD: &str = "after";

/// The field naming the object a download wants. A field rather than a path
/// segment, because an object key holds `/` and would otherwise have to be
/// reassembled from the route.
pub const KEY_FIELD: &str = "key";

/// One page of the archive listing. Keys and nothing else: the pool, the agent
/// and the kind are segments of the key, so a client filters a listing without
/// the server parsing one.
#[derive(Debug, Serialize, Deserialize)]
pub struct Listing {
    pub keys: Vec<String>,
    /// There is more after the last key (`metsuke_server::archive::Page`).
    pub truncated: bool,
}

/// An agent bounded whole-request by `timeout`, which covers connect, send and
/// read together, and which hands a status back as an answer rather than an
/// error, because a refusal is a reason to report and `classify` reads it.
pub fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .build()
        .into()
}

/// A non-2xx answer, its body read as the reason and reduced to one line
/// (`one_line`). `retryable` separates an endpoint that may come back from a
/// refusal that will answer the same way twice.
#[derive(Debug)]
pub struct Refusal {
    pub status: u16,
    pub reason: String,
    pub retryable: bool,
}

/// The one 4xx a retry answers differently: the window it names rolls whether
/// or not anything is changed.
const TOO_MANY_REQUESTS: u16 = 429;

/// Split a 2xx from everything else, leaving a success's body unread for the
/// caller. A 4xx is terminal: credentials, policy or a clock, which retrying
/// delays the reason for without changing it. Everything else may answer
/// differently next time, `TOO_MANY_REQUESTS` included.
pub fn classify(response: &mut ureq::http::Response<ureq::Body>) -> Result<(), Refusal> {
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(Refusal {
        status,
        reason: one_line(
            response
                .body_mut()
                .read_to_string()
                .unwrap_or_else(|error| format!("unreadable reason: {error}")),
        ),
        retryable: status == TOO_MANY_REQUESTS || !(400..500).contains(&status),
    })
}

/// How much of a refusal's body is worth repeating. metsuke-server answers one
/// sentence; this is for everything else that can answer instead.
const REASON_MAX_CHARS: usize = 200;

/// The body as one bounded line, because a refusal is logged and a log line is
/// a line. metsuke-server states a reason in a sentence, but anything between
/// an agent and it can answer first, and a proxy's HTML error page would
/// otherwise reach the journal as a screenful of markup whose newlines leave
/// every entry after the first without the severity prefix that was written
/// once, at the front.
fn one_line(body: String) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match collapsed.char_indices().nth(REASON_MAX_CHARS) {
        None => collapsed,
        // On a character boundary: a reason is whatever answered, so it is not
        // this crate's to assume it is ASCII.
        Some((cut, _)) => format!("{}…", &collapsed[..cut]),
    }
}
