//! What the server answers, as values: the POST route submissions arrive on,
//! whose headers decode into an `authority::Signed`, the two GET routes a
//! developer pulls the archive back out through (`developer`), and the
//! `instructions` page.
//!
//! No transport type appears here. `answer` takes a decoded `Request` and
//! returns an `Answer`, so every route's answer is reachable from a test
//! without a socket; `serve` is the only module that knows what carried it,
//! and words the refusals only it can reach.
//!
//! TLS belongs to the reverse proxy in front of this
//! (docs/research/endpoint-protection.md, Transport), as does any IP-keyed
//! limit (same doc, cost asymmetry and abuse handling) — this layer knows only
//! pool ids and one developer credential. The proxy is defence in depth and
//! nothing more: what the server refuses on its own it refuses with nothing in
//! front of it (`config::HttpConfig`).

use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY, PoolId, Signature, VerifyingKey};
use metsuke_wire::hex::{self, HexError};
use metsuke_wire::journal::{ERR, WARNING};
use time::OffsetDateTime;

use crate::archive::{ArchiveError, Bytes, List, ObjectStream, Store};
use crate::authority::Signed;
use crate::developer::{self, Developer, Filters, Unauthorized};
use crate::instructions;
use crate::intake::{IngestError, Intake, Rejection};

/// Where submissions arrive.
pub const SUBMIT_PATH: &str = "/v1/submit";

/// The developer routes and the field a download names its object in
/// (`metsuke_wire::http`), re-exported so a route reads off one name here.
pub use metsuke_wire::http::{KEY_FIELD, OBJECT_PATH, SUBMISSIONS_PATH};

/// The two methods the four routes take. Anything else is one value: a refusal
/// names the method the route accepts, never the one that was tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Other,
}

/// A request with its transport decoded.
#[derive(Debug)]
pub struct Request {
    pub method: Method,
    /// The target as sent, path and query together: the filters and the object
    /// key are read off it (`developer::Filters`).
    pub target: String,
    /// The two ADR-0001 headers, decoded by whoever built this — once, because
    /// whether they decode is also what decides if a body is worth reading
    /// (`serve::handle`). `Err` names the first header that did not decode.
    pub submission: Result<SubmissionHeaders, HeaderError>,
    pub authorization: Option<String>,
    pub body: Vec<u8>,
}

impl Request {
    /// The route the target names, which is the target up to its query.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
}

/// The identity a request presents. Holding one means the two ADR-0001 headers
/// were present and well formed; whether the signature verifies is the
/// intake's answer, not this type's.
#[derive(Debug)]
pub struct SubmissionHeaders {
    pub vkey: VerifyingKey,
    pub signature: Signature,
}

#[derive(Debug, thiserror::Error)]
pub enum HeaderError {
    #[error("{header} header is missing")]
    Missing { header: &'static str },
    #[error("{header} is not hex")]
    NotHex { header: &'static str },
    #[error("{header} decodes to {found} bytes, expected {expected}")]
    WrongLength {
        header: &'static str,
        found: usize,
        expected: usize,
    },
    #[error("{HEADER_VKEY} is not an Ed25519 verification key: {reason}")]
    Vkey { reason: String },
}

impl SubmissionHeaders {
    pub fn decode(
        vkey: Option<&str>,
        signature: Option<&str>,
    ) -> Result<SubmissionHeaders, HeaderError> {
        let vkey: [u8; 32] = decode_hex(vkey, HEADER_VKEY)?;
        let signature: [u8; 64] = decode_hex(signature, HEADER_SIGNATURE)?;
        Ok(SubmissionHeaders {
            vkey: VerifyingKey::from_bytes(&vkey).map_err(|error| HeaderError::Vkey {
                reason: error.to_string(),
            })?,
            signature: Signature::from_bytes(&signature),
        })
    }

    /// Whose upload this is (`authority`). Derived, so it holds before the
    /// signature is checked and says nothing about whether it will pass.
    pub fn pool_id(&self) -> PoolId {
        PoolId::from_cold_key(&self.vkey)
    }
}

/// Carries which header was wrong into `HeaderError`, so a refusal names the
/// one the operator has to fix.
fn decode_hex<const N: usize>(
    value: Option<&str>,
    header: &'static str,
) -> Result<[u8; N], HeaderError> {
    let value = value.ok_or(HeaderError::Missing { header })?;
    hex::decode(value).map_err(|error| match error {
        HexError::NotHex => HeaderError::NotHex { header },
        HexError::WrongLength { found, expected } => HeaderError::WrongLength {
            header,
            found,
            expected,
        },
    })
}

/// The status a rejected submission answers with. Takes a `Rejection` and not
/// an `IngestError`, so what this can answer is only ever the client's fault
/// and only ever 4xx: a rejection's text is the client's to read, where an
/// availability failure's is `unavailable`'s to withhold. What the 4xx/5xx
/// split means to the agent is ADR 0004; the finer codes are what an operator
/// reads off the proxy log.
pub fn status_for(rejection: &Rejection) -> u16 {
    match rejection {
        Rejection::OversizedBody { .. } => 413,
        Rejection::RateLimited { .. } | Rejection::ServerBusy { .. } => 429,
        Rejection::UnknownPool { .. } | Rejection::BadSignature => 403,
        Rejection::NotASubmission(_)
        | Rejection::UnreadableHeader(_)
        | Rejection::KeylessSchema { .. } => 400,
    }
}

/// What the server writes back. `headers` carries what a status needs beside
/// the body, which so far is the 401's challenge. Whose request it was is a
/// parameter of the refusal, not a field: nothing a client reads carries it,
/// and `serve` already holds it (`serve::handle`).
pub struct Answer {
    pub status: u16,
    pub content_type: &'static str,
    pub body: AnswerBody,
    pub headers: Vec<(&'static str, String)>,
}

/// What an answer's body is made of. Every refusal and every generated page is
/// bytes this server already holds; only the download is an archive read
/// (`archive::Bytes`).
pub enum AnswerBody {
    Bytes(bytes::Bytes),
    Stream(ObjectStream),
}

/// One request answered. Blocking, because it stores to and reads from the
/// archive (`serve::bind`).
pub fn answer<A: Store + Bytes + List>(
    intake: &Intake<A>,
    developer: &Developer,
    page: &bytes::Bytes,
    request: Request,
) -> Answer {
    let path = request.path().to_string();
    match path.as_str() {
        // Unauthenticated: it is what an operator reads before they have
        // anything to authenticate with.
        instructions::PATH => match request.method {
            Method::Get => Answer {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: AnswerBody::Bytes(page.clone()),
                headers: Vec::new(),
            },
            _ => refuse(None, 405, format!("{} takes GET", instructions::PATH)),
        },
        // A known route reached with the wrong method is named, because a
        // client that guessed the method is not a client at the wrong address.
        SUBMIT_PATH => match request.method {
            Method::Post => submit(intake, &request),
            _ => refuse(None, 405, format!("{SUBMIT_PATH} takes POST")),
        },
        // Authentication comes before the method check: answering 405 first
        // would confirm the route exists to a client that never presented a
        // credential.
        SUBMISSIONS_PATH | OBJECT_PATH => {
            if let Err(error) = developer.authorize(request.authorization.as_deref()) {
                return challenge(&path, &error);
            }
            if request.method != Method::Get {
                return refuse(None, 405, format!("{path} takes GET"));
            }
            match path.as_str() {
                SUBMISSIONS_PATH => listing(intake, developer, &request.target),
                _ => object(intake, &request.target),
            }
        }
        _ => refuse(
            None,
            404,
            format!("no route {path}, submissions go to {SUBMIT_PATH}"),
        ),
    }
}

/// Everything a 401 says to the client. One string for every reason, so the
/// body cannot tell a wrong password from a missing header.
pub const UNAUTHORIZED_BODY: &str = "credentials required";

/// The 401 both developer routes answer. Carries the challenge, so a browser
/// or `curl -u` knows what to send, and puts the reason in the log alone.
fn challenge(path: &str, error: &Unauthorized) -> Answer {
    let mut answer = refuse_withholding(
        None,
        401,
        UNAUTHORIZED_BODY.to_string(),
        Some(format!("{path}: {error}")),
    );
    answer.headers.push((
        "www-authenticate",
        format!("Basic realm=\"{}\", charset=\"UTF-8\"", developer::REALM),
    ));
    answer
}

/// One page of the archive listing as JSON, bounded by `list_max_rows`. The
/// filters go upstream as they arrived and the answer says whether there is
/// more after the last key (`archive::Page`).
fn listing<A: Store + Bytes + List>(
    intake: &Intake<A>,
    developer: &Developer,
    target: &str,
) -> Answer {
    let filters = match Filters::parse(target) {
        Ok(filters) => filters,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    match intake
        .archive()
        .page(&filters.prefix, &filters.after, developer.list_max_rows())
    {
        Ok(listing) => Answer {
            status: 200,
            content_type: "application/json",
            body: AnswerBody::Bytes(developer::page(listing).into_bytes().into()),
            headers: Vec::new(),
        },
        Err(error) => unavailable(None, "the archive cannot be listed", &error.to_string()),
    }
}

/// One object, as the bytes that were archived. No existence check first: the
/// archive is the only account of what it holds, so its own 404 is the answer.
fn object<A: Store + Bytes>(intake: &Intake<A>, target: &str) -> Answer {
    let key = match developer::query_value(target, KEY_FIELD) {
        Ok(key) => key,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    if key.is_empty() {
        return refuse(None, 400, format!("name the object in ?{KEY_FIELD}="));
    }
    match intake.archive().reader(&key) {
        Ok(stream) => Answer {
            status: 200,
            // RFC 8878: the body is the zstd the pool signed, so a developer
            // decompresses it themselves.
            content_type: "application/zstd",
            body: AnswerBody::Stream(stream),
            headers: Vec::new(),
        },
        Err(ArchiveError::NoSuchObject { .. }) => refuse(None, 404, "no such object".to_string()),
        Err(error) => unavailable(None, "the archive cannot be read", &error.to_string()),
    }
}

/// The 5xx every use of the archive answers with: a body saying which use
/// failed, and a log line saying how. The detail names the bucket, the
/// endpoint or the archive root, and none of that is a client's to read — a
/// pool id is public, so being on the allowlist is no reason to be told.
fn unavailable(signer: Option<PoolId>, reason: &str, withheld: &str) -> Answer {
    refuse_withholding(signer, 503, reason.to_string(), Some(withheld.to_string()))
}

fn submit<A: Store>(intake: &Intake<A>, request: &Request) -> Answer {
    let headers = match &request.submission {
        Ok(headers) => headers,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    let signer = headers.pool_id();
    let submission = Signed {
        vkey: headers.vkey,
        signature: headers.signature,
        wire_bytes: &request.body,
    };
    match intake.submit(&submission, OffsetDateTime::now_utc()) {
        Ok(ack) => Answer {
            status: 200,
            content_type: "application/json",
            body: AnswerBody::Bytes(
                serde_json::to_vec(&ack)
                    .expect("an Ack of two strings serializes")
                    .into(),
            ),
            headers: Vec::new(),
        },
        Err(IngestError::Rejected(rejection)) => {
            refuse(Some(signer), status_for(&rejection), rejection.to_string())
        }
        Err(IngestError::Archive(error)) => unavailable(
            Some(signer),
            "the archive cannot be written",
            &error.to_string(),
        ),
    }
}

/// The 413 an oversized body earns, worded by the intake so the client reads
/// one sentence for the cap whichever layer caught it. `serve` is the layer
/// that catches it, because it is the one reading the body.
pub fn oversized(signer: Option<PoolId>, found: usize, max: u64) -> Answer {
    refuse(
        signer,
        413,
        Rejection::OversizedBody { found, max }.to_string(),
    )
}

/// Every refusal is logged: the reason text is the only record of why a
/// pool's uploads are not landing.
pub fn refuse(signer: Option<PoolId>, status: u16, reason: String) -> Answer {
    refuse_withholding(signer, status, reason, None)
}

/// A refusal whose log line carries more than its body: `withheld` is appended
/// to what is logged and never sent.
fn refuse_withholding(
    signer: Option<PoolId>,
    status: u16,
    reason: String,
    withheld: Option<String>,
) -> Answer {
    let severity = if status >= 500 { ERR } else { WARNING };
    let logged = match &withheld {
        Some(withheld) => format!("{reason}: {withheld}"),
        None => reason.clone(),
    };
    eprintln!("{severity}refused {}: {status}, {logged}", named(signer));
    Answer {
        status,
        content_type: "text/plain; charset=utf-8",
        body: AnswerBody::Bytes(reason.into_bytes().into()),
        headers: Vec::new(),
    }
}

/// How a request is named before anything it claims has been verified.
pub fn named(signer: Option<PoolId>) -> String {
    match signer {
        Some(pool_id) => pool_id.to_bech32(),
        None => "an unidentified client".to_string(),
    }
}
