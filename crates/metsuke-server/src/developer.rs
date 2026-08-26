//! Developer pull access to the archive (ticket metsuke-4zo.10): one account,
//! a listing off the index, and objects handed back untransformed
//! (`archive::Bytes`).
//!
//! Why one shared account rather than a user per developer: the alternative is
//! a credential store this server has no other use for, and the bytes it
//! guards are self-verifying. What the credential decides is who may pull the
//! corpus, not whether it can be trusted.

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest as _};
use serde::Serialize;

use base64::Engine as _;

use crate::archive::{KEY_PREFIX, Page};
use crate::config::DeveloperConfig;

/// The realm a 401 names. Fixed: an operator changing it would only change
/// what a browser's prompt says.
pub const REALM: &str = "metsuke archive";

/// Keys one ListObjectsV2 answers with, which is S3's cap and so this
/// server's. A listing request is one upstream request, so a bound above it
/// would report a page as the whole archive.
pub const LIST_MAX_ROWS_CAP: std::num::NonZeroU32 = std::num::NonZeroU32::new(1000).unwrap();

/// The one account developer pulls authenticate as. Holds the credential
/// hashed, so a heap dump of a serving process does not carry the password.
pub struct Developer {
    credential: [u8; 32],
    list_max_rows: std::num::NonZeroU32,
}

/// A request that may not read the archive. The reason is for the operator's
/// log and nothing else — told which half of the credential was wrong, a
/// client learns whether the user exists — so the caller that answers is what
/// keeps it out of the body (`http::challenge`).
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Unauthorized(&'static str);

/// Blake2b-256 over `user:password`, which is the byte string Basic auth
/// carries. Comparing digests rather than the secrets is what keeps the
/// compare's timing independent of how much of the password matched, without a
/// constant-time-compare dependency: the bytes it does compare are a hash's,
/// and reveal nothing about their input.
fn digest(credential: &str) -> [u8; 32] {
    let mut hash = Blake2b::<U32>::new();
    hash.update(credential.as_bytes());
    hash.finalize().into()
}

impl Developer {
    pub fn new(config: &DeveloperConfig, password: &str) -> Developer {
        Developer {
            credential: digest(&format!("{}:{password}", config.user)),
            list_max_rows: config.list_max_rows,
        }
    }

    /// Keys one listing answers with, never more than one upstream page.
    /// Clamped rather than refused at load: `LIST_MAX_ROWS_CAP` is the
    /// protocol's, not the operator's, and a config that asks for more is
    /// asking for the most a page can hold.
    pub fn list_max_rows(&self) -> std::num::NonZeroU32 {
        self.list_max_rows.min(LIST_MAX_ROWS_CAP)
    }

    /// Whether `authorization` presents this account's credential. Everything
    /// malformed is refused exactly as a wrong credential is, and differs only
    /// in what the refusal says to the log.
    pub fn authorize(&self, authorization: Option<&str>) -> Result<(), Unauthorized> {
        let presented =
            basic_credential(authorization.ok_or(Unauthorized("no authorization header"))?)?;
        match digest(&presented) == self.credential {
            true => Ok(()),
            false => Err(Unauthorized("the credential is not this account's")),
        }
    }
}

/// The `user:password` a Basic header carries, or `Unauthorized` for anything
/// that is not one. A credential with no colon in it is not a credential:
/// RFC 7617 makes the first colon the separator, so its absence means the
/// client sent something else.
fn basic_credential(authorization: &str) -> Result<String, Unauthorized> {
    let (scheme, encoded) = authorization
        .split_once(' ')
        .ok_or(Unauthorized("the authorization header names no scheme"))?;
    if !scheme.eq_ignore_ascii_case("Basic") {
        return Err(Unauthorized("the authorization scheme is not Basic"));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Unauthorized("the Basic credential is not base64"))?;
    let credential = String::from_utf8(decoded)
        .map_err(|_| Unauthorized("the Basic credential is not UTF-8"))?;
    match credential.contains(':') {
        true => Ok(credential),
        false => Err(Unauthorized("the Basic credential holds no colon")),
    }
}

/// The two filters a listing request may carry.
#[derive(Debug)]
pub struct Filters {
    /// The literal head of an object key, which is what makes it a
    /// day-and-pool filter (`archive::ObjectName`).
    pub prefix: String,
    /// The last key of the previous page. Empty is the archive's start.
    pub after: String,
}

/// A query this layer will not act on.
#[derive(Debug, thiserror::Error)]
pub enum BadQuery {
    #[error("{field} is not UTF-8 once its percent escapes are decoded")]
    NotUtf8 { field: &'static str },
}

/// One query field of a request URL, percent-decoded, empty when absent. The
/// `/` an object key is made of is what a client that builds URLs properly
/// encodes, so decoding is not optional.
pub fn query_value(url: &str, field: &'static str) -> Result<String, BadQuery> {
    let Some(raw) = url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == field)
        .map(|(_, value)| value)
    else {
        return Ok(String::new());
    };
    percent_decoded(raw).ok_or(BadQuery::NotUtf8 { field })
}

impl Filters {
    /// Read the filters off a request URL. An absent prefix becomes the
    /// archive's own, so "everything" is every object this server filed
    /// rather than every key in the bucket.
    pub fn parse(url: &str) -> Result<Filters, BadQuery> {
        let asked = query_value(url, "prefix")?;
        Ok(Filters {
            prefix: match asked.is_empty() {
                true => KEY_PREFIX.to_string(),
                false => asked,
            },
            after: query_value(url, "after")?,
        })
    }
}

/// `%XX` decoded, or `None` when the bytes that come out are not UTF-8.
/// Refused rather than replaced: a lossy decode would answer a query the
/// client did not send, and answer it with a 200.
///
/// A malformed escape — half-written, or complete but not hex — stays as
/// written. It cannot be part of an object key either way, so it fails at the
/// filter rather than silently becoming a different string.
fn percent_decoded(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes
            .get(index + 1..index + 3)
            .filter(|_| bytes[index] == b'%')
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

/// One page of the archive as the JSON a developer parses. Keys and nothing
/// else: the pool, the agent and the kind are segments of the key
/// (`archive::ObjectName`), and the server does not parse what the bucket
/// hands it back.
#[derive(Debug, Serialize)]
struct PageJson<'a> {
    keys: &'a [String],
    truncated: bool,
}

pub fn page(listing: &Page) -> String {
    let page = PageJson {
        keys: &listing.keys,
        truncated: listing.truncated,
    };
    serde_json::to_string(&page).expect("a page of keys and a flag serializes")
}
