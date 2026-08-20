//! The production archive: one PUT per accepted submission, synchronous
//! before the ACK (ADR 0004).
//!
//! Requests are presigned because that is the seam rusty-s3 offers — it
//! returns a signed URL, and ureq has no hook for signing headers as they go
//! out.

use std::time::Duration;

use rusty_s3::actions::{ListObjectsV2, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};

use metsuke::envelope::{Signature, VerifyingKey};

use crate::WARNING;
use crate::archive::{
    Archive, ArchiveError, Fetch, FetchedObject, KEY_PREFIX, ObjectName, StoredSubmission,
};
use crate::config::S3Config;

/// The metadata an object carries beside its bytes. These four plus the key's
/// pool id are the whole verification input (ADR 0005).
pub const META_SIGNATURE: &str = "x-amz-meta-signature";
pub const META_VKEY: &str = "x-amz-meta-vkey";
pub const META_COUNTER: &str = "x-amz-meta-counter";
pub const META_SCHEMA_VERSION: &str = "x-amz-meta-schema-version";

/// `Debug` is safe to derive: `Credentials` prints its key id and withholds
/// the secret.
#[derive(Debug)]
pub struct S3Archive {
    bucket: Bucket,
    credentials: Credentials,
    agent: ureq::Agent,
    request_timeout: Duration,
    /// Spent only on failures that can clear on their own; the client keeps
    /// spooling either way (ADR 0004).
    put_retries: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum S3Error {
    #[error("endpoint {endpoint:?} is not an S3 endpoint: {reason}")]
    Endpoint { endpoint: String, reason: String },
}

impl S3Archive {
    pub fn new(config: &S3Config, credentials: Credentials) -> Result<S3Archive, S3Error> {
        let refuse = |reason: String| S3Error::Endpoint {
            endpoint: config.endpoint.clone(),
            reason,
        };
        let endpoint = config
            .endpoint
            .parse()
            .map_err(|error: url::ParseError| refuse(error.to_string()))?;
        // Path style: the bucket name stays in the path, which is what an
        // S3-compatible endpoint without wildcard DNS (Garage, MinIO) serves.
        let bucket = Bucket::new(
            endpoint,
            UrlStyle::Path,
            config.bucket.clone(),
            config.region.clone(),
        )
        .map_err(|error| refuse(error.to_string()))?;
        let request_timeout = Duration::from_secs(config.request_timeout_secs);
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(request_timeout))
            // Status handling is ours: a refusal is a reason to report, not an
            // opaque error.
            .http_status_as_error(false)
            .build()
            .into();
        Ok(S3Archive {
            bucket,
            credentials,
            agent,
            request_timeout,
            put_retries: config.put_retries,
        })
    }

    /// PUT the body once. Every `x-amz-*` header sent is signed: S3 refuses a
    /// presigned request carrying one that is not.
    fn put(&self, key: &str, metadata: &[(&str, String)], body: &[u8]) -> Result<(), Failure> {
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        for (name, value) in metadata {
            action.headers_mut().insert(*name, value.clone());
        }
        let url = action.sign(self.request_timeout);
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
                retryable: true,
            })
    }
}

/// A single failed attempt. `retryable` is what separates an endpoint that may
/// come back from a refusal that will answer the same way twice.
struct Failure {
    reason: String,
    retryable: bool,
}

fn answer(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<ureq::http::Response<ureq::Body>, Failure> {
    let mut response = sent.map_err(|error| Failure {
        retryable: transient(&error),
        reason: error.to_string(),
    })?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let reason = response
        .body_mut()
        .read_to_string()
        .unwrap_or_else(|error| format!("unreadable reason: {error}"));
    Err(Failure {
        reason: format!("the endpoint answered {status}: {reason}"),
        // A 4xx is a misconfigured bucket, policy or clock; retrying it only
        // holds the client's request open longer.
        retryable: status >= 500,
    })
}

/// Whether a transport failure is worth a second attempt. A name that does not
/// resolve, a certificate that does not verify and a URL that will not parse
/// are deployment errors: retrying them costs the client a whole timeout
/// before it hears the reason.
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
            | ureq::Error::RedirectFailed
            | ureq::Error::TooManyRedirects
            | ureq::Error::BodyExceedsLimit(_)
    )
}

impl Archive for S3Archive {
    fn store(&self, submission: &StoredSubmission<'_>) -> Result<(), ArchiveError> {
        let key = submission.object_key();
        let metadata = [
            (META_SIGNATURE, hex(&submission.signature.to_bytes())),
            (META_VKEY, hex(submission.vkey.as_bytes())),
            (META_COUNTER, submission.counter.to_string()),
            (META_SCHEMA_VERSION, submission.schema_version.to_string()),
        ];
        let mut attempts = 0;
        loop {
            attempts += 1;
            match self.put(&key, &metadata, submission.wire_bytes) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.retryable && attempts <= self.put_retries => {
                    eprintln!("{WARNING}retrying the PUT of {key}: {}", failure.reason);
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

    fn list_keys(&self) -> Result<Vec<String>, ArchiveError> {
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(KEY_PREFIX);
            if let Some(token) = token {
                action.with_continuation_token(token);
            }
            let url = action.sign(self.request_timeout);
            let text = self.get(&url).map_err(|failure| ArchiveError::List {
                reason: failure.reason,
            })?;
            let page =
                ListObjectsV2::parse_response(&text).map_err(|error| ArchiveError::List {
                    reason: format!("the listing does not parse: {error}"),
                })?;
            keys.extend(page.contents.into_iter().map(|object| object.key));
            // A truncated listing hands back the token to resume from; the
            // rebuild needs every page or it seeds from a short corpus.
            match page.next_continuation_token {
                Some(next) => token = Some(next),
                None => return Ok(keys),
            }
        }
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
        let url = action.sign(self.request_timeout);
        let mut response = answer(self.agent.get(url.as_str()).call())
            .map_err(|failure| refuse(failure.reason))?;
        // Read the metadata out before the body: an object missing a header is
        // unverifiable however good its bytes are.
        let metadata = |header: &'static str| -> Result<&str, String> {
            response
                .headers()
                .get(header)
                .ok_or(format!("no {header} on the object"))?
                .to_str()
                .map_err(|_| format!("{header} is not text"))
        };
        let read = || -> Result<(VerifyingKey, Signature, u32, u64), String> {
            Ok((
                VerifyingKey::from_bytes(&unhex(metadata(META_VKEY)?, META_VKEY)?)
                    .map_err(|error| format!("{META_VKEY}: {error}"))?,
                Signature::from_bytes(&unhex(metadata(META_SIGNATURE)?, META_SIGNATURE)?),
                number(metadata(META_SCHEMA_VERSION)?, META_SCHEMA_VERSION)?,
                number(metadata(META_COUNTER)?, META_COUNTER)?,
            ))
        };
        let (vkey, signature, metadata_schema_version, metadata_counter) =
            read().map_err(refuse)?;
        let wire_bytes = response
            .body_mut()
            .read_to_vec()
            .map_err(|error| refuse(format!("unreadable body: {error}")))?;
        Ok(FetchedObject {
            name,
            vkey,
            signature,
            metadata_schema_version,
            metadata_counter,
            wire_bytes,
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fixed-width hex metadata value. Mirrors the header decoder in `http`,
/// which reads the same encoding off the upload (ticket metsuke-4zo.23).
fn unhex<const N: usize>(text: &str, header: &str) -> Result<[u8; N], String> {
    let refuse = || format!("{header} is not {N} bytes of hex: {text:?}");
    if text.len() != N * 2 {
        return Err(refuse());
    }
    let mut bytes = [0u8; N];
    for (byte, pair) in bytes.iter_mut().zip(text.as_bytes().chunks(2)) {
        let digits = std::str::from_utf8(pair).map_err(|_| refuse())?;
        *byte = u8::from_str_radix(digits, 16).map_err(|_| refuse())?;
    }
    Ok(bytes)
}

fn number<T: std::str::FromStr>(text: &str, header: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{header} is not a number: {text:?}"))
}
