//! The archive-pull routes as a client (`metsuke_wire::http`): one page of
//! keys, one object's bytes. Nothing here writes a file or reads a cursor.
//! There is only a request and what came back, so `sync` is testable as the
//! loop it is.

use std::io;
use std::num::NonZeroU64;
use std::time::Duration;

use base64::Engine as _;
use metsuke_wire::envelope::{Attestation, HEADER_SIGNATURE, HEADER_VKEY};
use metsuke_wire::http::{
    self, AFTER_FIELD, KEY_FIELD, Listing, OBJECT_PATH, PREFIX_FIELD, SUBMISSIONS_PATH,
};

/// The most an object's declared length may reserve before its body arrives.
/// Not a limit on the object, which is `--max-object-bytes`; this is how much
/// of a stranger's arithmetic the process acts on in advance.
///
/// A constant rather than configuration, against the convention: raising it
/// only buys back one `Vec` growth and hands a lying `Content-Length` that much
/// more of this process's memory, so there is nothing here worth exposing.
const PREALLOCATED_MAX: u64 = 1024 * 1024;

/// One object as it came back: the bytes to write, and the pair that says
/// whose they are where the archive held it. `None` is not a fault of the
/// download. A filesystem archive discards the pair at ingest
/// (`metsuke_server::archive::FilesystemArchive`), so an object stored through
/// one can never be checked by anybody, and an object written by something
/// other than metsuke-server carries none either.
pub struct Object {
    pub bytes: Vec<u8>,
    pub attestation: Option<Attestation>,
}

/// The two headers off an answer's head. What an unverifiable object means is
/// `sync`'s to say; `Attestation::from_headers` says when there is one.
fn attestation(response: &ureq::http::Response<ureq::Body>) -> Option<Attestation> {
    let text = |header: &str| -> Option<&str> { response.headers().get(header)?.to_str().ok() };
    Attestation::from_headers(text(HEADER_VKEY), text(HEADER_SIGNATURE))
}

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
    /// Refused before a byte is read. Checking an object means holding it, so
    /// this is the bound on what one download may cost, and raising
    /// `--max-object-bytes` is what an operator does about it.
    #[error("{key} declares {length} bytes, over the {max} byte limit this run will hold to check")]
    Oversized { key: String, length: u64, max: u64 },
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

    /// One object, verbatim, with whatever the answer carried to check it
    /// with. The count is checked against the length the server declared: a
    /// download cut short must not be written down as the object.
    ///
    /// Held whole rather than streamed to its file, because the signature is
    /// over the whole body (ADR 0001) and `verify_strict` takes a slice, so
    /// nothing can check an object it has only seen a chunk at a time. That is
    /// what `max_object_bytes` bounds, and it is why the length is refused
    /// before a byte is read rather than after.
    pub fn object(&self, key: &str, max_object_bytes: NonZeroU64) -> Result<Object, PullError> {
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
        if length > max_object_bytes.get() {
            return Err(PullError::Oversized {
                key: key.to_string(),
                length,
                max: max_object_bytes.get(),
            });
        }
        let attestation = attestation(&response);
        // A hint only, so the cap costs a reallocation on an object past it and
        // nothing else: `io::copy` grows this as the body arrives, and what
        // bounds the object is the refusal above. Capped because the length
        // here is the one the server declared, and reserving all of it would
        // let an answer that sends no body at all spend the memory of one.
        let mut bytes = Vec::with_capacity(length.min(PREALLOCATED_MAX) as usize);
        let read =
            io::copy(&mut response.body_mut().as_reader(), &mut bytes).map_err(|source| {
                PullError::Unread {
                    key: key.to_string(),
                    source,
                }
            })?;
        match read == length {
            true => Ok(Object { bytes, attestation }),
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
