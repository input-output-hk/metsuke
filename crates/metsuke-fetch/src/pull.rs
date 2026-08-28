//! The archive-pull routes as a client (`metsuke_wire::http`): one page of
//! keys, one object's bytes. Nothing here writes a file or reads a cursor.
//! There is only a request and what came back, so `sync` is testable as the
//! loop it is.

use std::io;
use std::time::Duration;

use base64::Engine as _;
use metsuke_wire::http::{
    self, AFTER_FIELD, KEY_FIELD, Listing, OBJECT_PATH, PREFIX_FIELD, SUBMISSIONS_PATH,
};

/// The archive behind one server and one account.
pub struct Archive {
    agent: ureq::Agent,
    /// The server as given, with no trailing slash, so a route appends to it.
    server: String,
    /// `Basic <base64>`, built once. Held rather than the password, because
    /// this is the only form it goes out in.
    authorization: String,
}

#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("{url} could not be reached: {reason}")]
    Unreachable { url: String, reason: String },
    /// A status the server chose. The reason is its own body, which is what
    /// says whether the credential, the route or the key was the problem.
    #[error("{url} answered {status}: {reason}")]
    Refused {
        url: String,
        status: u16,
        reason: String,
    },
    #[error("the listing from {url} does not parse: {reason}")]
    UnreadableListing { url: String, reason: String },
    /// The download route states one on every answer, so an answer without one
    /// is not this archive's (`metsuke_server::archive::ObjectStream`). Refused
    /// rather than read to the end: a body of unknown length cannot be told
    /// from one cut short.
    #[error("the download of {key} states no length")]
    NoLength { key: String },
    #[error("the download of {key} ended after {read} of the {length} bytes it declared")]
    Short { key: String, read: u64, length: u64 },
    #[error("the download of {key} did not read: {source}")]
    Unread {
        key: String,
        #[source]
        source: io::Error,
    },
}

impl Archive {
    pub fn new(server: &str, user: &str, password: &str, timeout: Duration) -> Archive {
        let credential =
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        Archive {
            agent: http::agent(timeout),
            server: server.trim_end_matches('/').to_string(),
            authorization: format!("Basic {credential}"),
        }
    }

    /// One page of the keys under `prefix` that sort after `after`, as the
    /// server bounds it. `after` empty is the archive's start.
    pub fn page(&self, prefix: &str, after: &str) -> Result<Listing, PullError> {
        let url = format!(
            "{}{SUBMISSIONS_PATH}?{PREFIX_FIELD}={}&{AFTER_FIELD}={}",
            self.server,
            escaped(prefix),
            escaped(after),
        );
        let mut response = self.answered(&url)?;
        let body =
            response
                .body_mut()
                .read_to_string()
                .map_err(|error| PullError::UnreadableListing {
                    url: url.clone(),
                    reason: error.to_string(),
                })?;
        serde_json::from_str(&body).map_err(|error| PullError::UnreadableListing {
            url,
            reason: error.to_string(),
        })
    }

    /// One object copied into `into`, verbatim, returning how many bytes that
    /// was. The count is checked against the length the server declared: a
    /// download cut short must not be written down as the object.
    pub fn object(&self, key: &str, into: &mut dyn io::Write) -> Result<u64, PullError> {
        let url = format!("{}{OBJECT_PATH}?{KEY_FIELD}={}", self.server, escaped(key));
        let mut response = self.answered(&url)?;
        // ureq's own reading of the length, which answers `None` for a chunked
        // or close-delimited body as well as for an absent header: a stated
        // length is only a length when the framing makes it one.
        let length = response
            .body()
            .content_length()
            .ok_or_else(|| PullError::NoLength {
                key: key.to_string(),
            })?;
        let read = io::copy(&mut response.body_mut().as_reader(), into).map_err(|source| {
            PullError::Unread {
                key: key.to_string(),
                source,
            }
        })?;
        match read == length {
            true => Ok(read),
            false => Err(PullError::Short {
                key: key.to_string(),
                read,
                length,
            }),
        }
    }

    /// A GET this account made, with a non-2xx turned into the refusal it is.
    fn answered(&self, url: &str) -> Result<ureq::http::Response<ureq::Body>, PullError> {
        let mut response = self
            .agent
            .get(url)
            .header("authorization", &self.authorization)
            .call()
            .map_err(|error| PullError::Unreachable {
                url: url.to_string(),
                reason: error.to_string(),
            })?;
        match http::classify(&mut response) {
            Ok(()) => Ok(response),
            Err(refusal) => Err(PullError::Refused {
                url: url.to_string(),
                status: refusal.status,
                reason: refusal.reason,
            }),
        }
    }
}

/// One query value, percent-escaped. Only the unreserved set survives as
/// itself: an object key holds `/` and a prefix is whatever the caller typed,
/// and the route reads both back through one decoder
/// (`metsuke_server::developer::percent_decoded`).
fn escaped(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                escaped.push(byte as char)
            }
            _ => escaped.push_str(&format!("%{byte:02X}")),
        }
    }
    escaped
}
