//! The production archive: one PUT per accepted submission, synchronous
//! before the ACK (ADR 0004).
//!
//! Requests are presigned because that is the seam rusty-s3 offers. It
//! returns a signed URL, and ureq has no hook for signing headers as they go
//! out.

use std::num::NonZeroU32;
use std::time::Duration;

use rusty_s3::actions::{ListObjectsV2, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};

use metsuke_wire::envelope::{Signature, VerifyingKey};
use metsuke_wire::{hex, http};

use crate::archive::{
    ArchiveError, Attestation, Bytes, Fetch, FetchedObject, KEY_PREFIX, List, ObjectName,
    ObjectStream, Page, Store, StoredSubmission,
};
use crate::config::S3Config;
use metsuke_wire::journal::WARNING;

/// The metadata an object carries beside its bytes: the two facts that are not
/// inside them, and the whole verification input with them (ADR 0005).
pub const META_SIGNATURE: &str = "x-amz-meta-signature";
pub const META_VKEY: &str = "x-amz-meta-vkey";

/// `Debug` is safe to derive: `Credentials` prints its key id and withholds
/// the secret.
#[derive(Debug)]
pub struct S3Archive {
    bucket: Bucket,
    credentials: Credentials,
    agent: ureq::Agent,
    signature_validity: Duration,
    put_retries: u32,
    put_retry_backoff: Duration,
    list_max_pages: NonZeroU32,
}

#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("endpoint {endpoint} is not an S3 endpoint: {reason}")]
    Endpoint { endpoint: url::Url, reason: String },
}

impl S3Archive {
    pub fn new(config: &S3Config, credentials: Credentials) -> Result<S3Archive, S3Error> {
        // Path style: the bucket name stays in the path, which is what an
        // S3-compatible endpoint without wildcard DNS (Garage, MinIO) serves.
        let bucket = Bucket::new(
            config.endpoint.clone(),
            UrlStyle::Path,
            config.bucket.clone(),
            config.region.clone(),
        )
        .map_err(|error| S3Error::Endpoint {
            endpoint: config.endpoint.clone(),
            reason: error.to_string(),
        })?;
        let agent = http::agent(Duration::from_millis(config.request_timeout_ms.get()));
        Ok(S3Archive {
            bucket,
            credentials,
            agent,
            signature_validity: Duration::from_secs(config.signature_validity_secs.get()),
            put_retries: config.put_retries,
            put_retry_backoff: Duration::from_millis(config.put_retry_backoff_ms.get()),
            list_max_pages: config.list_max_pages,
        })
    }

    /// PUT the body once. Every `x-amz-*` header sent is signed: S3 refuses a
    /// presigned request carrying one that is not.
    fn put(&self, key: &str, metadata: &[(&str, String)], body: &[u8]) -> Result<(), Failure> {
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        for (name, value) in metadata {
            action.headers_mut().insert(*name, value.clone());
        }
        let url = action.sign(self.signature_validity);
        let mut request = self.agent.put(url.as_str());
        for (name, value) in metadata {
            request = request.header(*name, value);
        }
        answer(request.send(body)).map(drop)
    }

    fn get(&self, url: &url::Url) -> Result<String, Failure> {
        let mut response = answer(self.agent.get(url.as_str()).call())?;
        response
            .body_mut()
            .read_to_string()
            .map_err(|error| Failure {
                reason: format!("unreadable answer: {error}"),
                // Same variant matching as the transport path: an answer over
                // the body limit reads the same way however often it is asked
                // for.
                retryable: transient(&error),
                status: None,
            })
    }
}

/// A single failed attempt, transport error and refusal alike, with
/// `retryable` carried through from `transient` or `http::Refusal`.
/// `status` is the endpoint's own, and `None` where the request never got an
/// answer to read one off.
struct Failure {
    reason: String,
    retryable: bool,
    status: Option<u16>,
}

fn answer(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, Failure> {
    let mut response = sent.map_err(|error| Failure {
        retryable: transient(&error),
        reason: error.to_string(),
        status: None,
    })?;
    match http::classify(&mut response) {
        Ok(()) => Ok(response),
        Err(refusal) => Err(Failure {
            reason: format!(
                "the endpoint answered {}: {}",
                refusal.status, refusal.reason
            ),
            retryable: refusal.retryable,
            status: Some(refusal.status),
        }),
    }
}

/// Whether a transport failure is worth a second attempt. A name that does not
/// resolve, a certificate that does not verify and a URL that will not parse
/// are deployment errors: retrying them costs the client a whole timeout
/// before it hears the reason. A redirect is not listed, because
/// `http::classify` reads a redirect that arrives as a status as retryable
/// and one condition cannot have two answers.
fn transient(error: &ureq::Error) -> bool {
    !matches!(
        error,
        ureq::Error::BadUri(_)
            | ureq::Error::Http(_)
            | ureq::Error::HostNotFound
            | ureq::Error::InvalidProxyUrl
            | ureq::Error::Tls(_)
            | ureq::Error::Pem(_)
            | ureq::Error::Rustls(_)
            | ureq::Error::BodyExceedsLimit(_)
    )
}

impl Store for S3Archive {
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError> {
        let key = submission.object_key();
        let metadata = [
            (
                META_SIGNATURE,
                hex::encode(&submission.signature.to_bytes()),
            ),
            (META_VKEY, hex::encode(submission.vkey.as_bytes())),
        ];
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.put(&key, &metadata, submission.wire_bytes) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.retryable && attempts <= self.put_retries => {
                    eprintln!("{WARNING}retrying the PUT of {key}: {}", failure.reason);
                    std::thread::sleep(self.put_retry_backoff);
                }
                Err(failure) => {
                    return Err(ArchiveError::Upload {
                        key,
                        attempts,
                        reason: failure.reason,
                    });
                }
            }
        }
    }
}

impl List for S3Archive {
    fn location(&self) -> String {
        format!("{} at {}", self.bucket.name(), self.bucket.base_url())
    }

    fn for_each_key<E: From<ArchiveError>>(
        &self,
        mut visit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        let refuse = |reason: String| E::from(ArchiveError::List { reason });
        let mut token: Option<String> = None;
        for page_number in 1..=self.list_max_pages.get() {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(KEY_PREFIX);
            if let Some(token) = &token {
                action.with_continuation_token(token.clone());
            }
            let url = action.sign(self.signature_validity);
            let text = self.get(&url).map_err(|failure| refuse(failure.reason))?;
            let page = ListObjectsV2::parse_response(&text)
                .map_err(|error| refuse(format!("the listing does not parse: {error}")))?;
            for object in &page.contents {
                visit(&object.key)?;
            }
            // A truncated listing hands back the token to resume from; the
            // rebuild needs every page or it seeds from a short corpus. A token
            // that does not advance would resume the same page forever, and a
            // hang is worse news than a bound that fails.
            match page.next_continuation_token {
                None => return Ok(()),
                Some(next) if Some(&next) == token.as_ref() => {
                    return Err(refuse(format!(
                        "page {page_number} handed back its own continuation token {next:?}"
                    )));
                }
                Some(next) => token = Some(next),
            }
        }
        Err(refuse(format!(
            "the listing is still truncated after {} pages (list_max_pages)",
            self.list_max_pages
        )))
    }

    /// One ListObjectsV2, passed through: the client's prefix and cursor go
    /// upstream as `prefix` and `start-after`, and `truncated` is the
    /// endpoint's own answer. `max_keys` is already at or under S3's per-page
    /// cap (`developer::Developer::list_max_rows`), so one request is one
    /// page and nothing here paginates.
    fn page(&self, prefix: &str, after: &str, max_keys: NonZeroU32) -> Result<Page, ArchiveError> {
        let refuse = |reason: String| ArchiveError::List { reason };
        let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
        // Verbatim: an object key carries the archive's own prefix, and
        // `Filters::parse` is what keeps a client's empty prefix inside it.
        action.with_prefix(prefix.to_string());
        action.with_max_keys(max_keys.get() as usize);
        if !after.is_empty() {
            action.with_start_after(after.to_string());
        }
        let url = action.sign(self.signature_validity);
        let text = self.get(&url).map_err(|failure| refuse(failure.reason))?;
        let listing = ListObjectsV2::parse_response(&text)
            .map_err(|error| refuse(format!("the listing does not parse: {error}")))?;
        Ok(Page {
            keys: listing
                .contents
                .into_iter()
                .map(|object| object.key)
                .collect(),
            // rusty-s3 drops `IsTruncated`; the continuation token stands in
            // for it, S3 returning one exactly when there is more after this
            // page.
            truncated: listing.next_continuation_token.is_some(),
        })
    }
}

impl Bytes for S3Archive {
    fn reader(&self, key: &str) -> Result<ObjectStream, ArchiveError> {
        let refuse = |reason: String| ArchiveError::Fetch {
            key: key.to_string(),
            reason,
        };
        // Parsed before it is signed into a URL: nothing but a v1 object key
        // reaches the bucket, whatever a client asked for.
        ObjectName::parse(key).map_err(|error| refuse(error.to_string()))?;
        let url = self
            .bucket
            .get_object(Some(&self.credentials), key)
            .sign(self.signature_validity);
        // The bucket is the source of truth about what it holds, so its 404
        // is the answer rather than something to pre-empt with a lookup.
        let response =
            answer(self.agent.get(url.as_str()).call()).map_err(|failure| {
                match failure.status {
                    Some(404) => ArchiveError::NoSuchObject {
                        key: key.to_string(),
                    },
                    _ => refuse(failure.reason),
                }
            })?;
        // Off the head, before the body is taken: what a consumer checks the
        // bytes with travels with them or the download is unverifiable. An
        // object without it was not written by this server, and saying so is
        // the client's to do, so it goes out absent rather than as a refusal.
        let attestation = attestation(&response).ok();
        let body = response.into_body();
        // Why a download cannot go out without one: `archive::ObjectStream`.
        let length = body
            .content_length()
            .ok_or_else(|| ArchiveError::EndpointUnusable {
                endpoint: self.bucket.base_url().to_string(),
            })?;
        Ok(ObjectStream {
            key: key.to_string(),
            length,
            attestation,
            reader: Box::new(body.into_reader()),
        })
    }
}

impl Fetch for S3Archive {
    fn fetch(&self, key: &str) -> Result<FetchedObject, ArchiveError> {
        let refuse = |reason: String| ArchiveError::Fetch {
            key: key.to_string(),
            reason,
        };
        let name = ObjectName::parse(key).map_err(|error| refuse(error.to_string()))?;
        let action = self.bucket.get_object(Some(&self.credentials), key);
        let url = action.sign(self.signature_validity);
        let mut response = answer(self.agent.get(url.as_str()).call())
            .map_err(|failure| refuse(failure.reason))?;
        // Read the metadata out before the body: an object missing a header is
        // unverifiable however good its bytes are.
        let Attestation { vkey, signature } = attestation(&response).map_err(refuse)?;
        let wire_bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|error| refuse(format!("unreadable body: {error}")))?;
        Ok(FetchedObject {
            name,
            vkey,
            signature,
            wire_bytes,
        })
    }
}

/// The two metadata headers an object carries, off a GET answer's head. Named
/// as the failure it is rather than as an `Option`, because an object this
/// server stored has both (ADR 0005): a download tolerates their absence and
/// an audit does not, so which it is belongs to the caller.
fn attestation(response: &ureq::http::Response<ureq::Body>) -> Result<Attestation, String> {
    let metadata = |header: &'static str| -> Result<&str, String> {
        response
            .headers()
            .get(header)
            .ok_or(format!("no {header} on the object"))?
            .to_str()
            .map_err(|_| format!("{header} is not text"))
    };
    Ok(Attestation {
        vkey: VerifyingKey::from_bytes(&unhex(metadata(META_VKEY)?, META_VKEY)?)
            .map_err(|error| format!("{META_VKEY}: {error}"))?,
        signature: Signature::from_bytes(&unhex(metadata(META_SIGNATURE)?, META_SIGNATURE)?),
    })
}

/// Carries which metadata header was wrong into `fetch`'s reason string.
fn unhex<const N: usize>(text: &str, header: &str) -> Result<[u8; N], String> {
    hex::decode(text).map_err(|error| format!("{header} is not hex: {error} ({text:?})"))
}
