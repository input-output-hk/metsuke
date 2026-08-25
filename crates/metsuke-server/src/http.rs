//! The HTTP surface: the POST route submissions arrive on, whose headers
//! decode into an `authority::Signed`, the two GET routes a developer pulls
//! the archive back out through (`developer`), and the `instructions` page.
//! TLS belongs to the reverse proxy in front of this (endpoint-protection.md,
//! Transport), as does any IP-keyed limit (same doc, cost asymmetry and abuse
//! handling) — this layer knows only pool ids and one developer credential.
//!
//! What the proxy must also do: buffer each request body in full before
//! forwarding it. `serve` reads bodies on the accepting thread and tiny_http
//! 0.12 exposes no socket timeout, so one client trickling a body stalls
//! ingest for every pool until it gives up. Binding a loopback address is
//! what keeps that reachable only through the proxy.

use std::io::Read;

use metsuke_wire::envelope::{
    HEADER_POOL_ID, HEADER_SIGNATURE, HEADER_VKEY, PoolId, PoolIdError, Signature, VerifyingKey,
};
use metsuke_wire::hex::{self, HexError};
use metsuke_wire::journal::{ERR, WARNING};
use time::OffsetDateTime;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::archive::{Bytes, Store};
use crate::authority::{Authority, Signed};
use crate::developer::{self, Developer, Filters, Unauthorized};
use crate::index::IndexError;
use crate::instructions;
use crate::intake::{IngestError, Intake, Rejection};

/// Where submissions arrive.
pub const SUBMIT_PATH: &str = "/v1/submit";

/// The developer routes: one page of the index, and one object's bytes.
pub const SUBMISSIONS_PATH: &str = "/v1/submissions";
pub const OBJECT_PATH: &str = "/v1/object";

/// The query field naming the object a download wants. A field rather than a
/// path segment, because an object key holds `/` and would otherwise have to
/// be reassembled from the route.
pub const KEY_FIELD: &str = "key";

/// The identity a request claims. Holding one means the three ADR-0001
/// headers were present and well formed; whether the signature verifies is
/// the intake's answer, not this type's.
#[derive(Debug)]
pub struct SubmissionHeaders {
    pub pool_id: PoolId,
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
    #[error("{HEADER_POOL_ID}: {0}")]
    PoolId(#[from] PoolIdError),
    #[error("{HEADER_VKEY} is not an Ed25519 verification key: {reason}")]
    Vkey { reason: String },
}

impl SubmissionHeaders {
    pub fn decode(headers: &[Header]) -> Result<SubmissionHeaders, HeaderError> {
        let pool_id = PoolId::from_bech32(value(headers, HEADER_POOL_ID)?)?;
        let vkey: [u8; 32] = decode_hex(headers, HEADER_VKEY)?;
        let signature: [u8; 64] = decode_hex(headers, HEADER_SIGNATURE)?;
        Ok(SubmissionHeaders {
            pool_id,
            vkey: VerifyingKey::from_bytes(&vkey).map_err(|error| HeaderError::Vkey {
                reason: error.to_string(),
            })?,
            signature: Signature::from_bytes(&signature),
        })
    }
}

fn value<'a>(headers: &'a [Header], header: &'static str) -> Result<&'a str, HeaderError> {
    headers
        .iter()
        .find(|candidate| candidate.field.equiv(header))
        .map(|candidate| candidate.value.as_str())
        .ok_or(HeaderError::Missing { header })
}

/// Carries which header was wrong into `HeaderError`, so a refusal names the
/// one the operator has to fix.
fn decode_hex<const N: usize>(
    headers: &[Header],
    header: &'static str,
) -> Result<[u8; N], HeaderError> {
    hex::decode(value(headers, header)?).map_err(|error| match error {
        HexError::NotHex => HeaderError::NotHex { header },
        HexError::WrongLength { found, expected } => HeaderError::WrongLength {
            header,
            found,
            expected,
        },
    })
}

/// The status a failed submission answers with. What the 4xx/5xx split means
/// to the agent is ADR 0004; the finer codes are what an operator reads off
/// the proxy log.
pub fn status_for(error: &IngestError) -> u16 {
    match error {
        IngestError::Rejected(rejection) => match rejection {
            Rejection::OversizedBody { .. } | Rejection::OversizedPayload { .. } => 413,
            Rejection::RateLimited { .. } => 429,
            Rejection::UnknownPool { .. }
            | Rejection::UnauthorizedKey { .. }
            | Rejection::BadSignature => 403,
            Rejection::MalformedPayload { .. }
            | Rejection::UnsupportedSchema { .. }
            | Rejection::PoolIdMismatch { .. }
            | Rejection::TimestampOutOfWindow { .. }
            | Rejection::ReplayedCounter { .. } => 400,
        },
        IngestError::CounterState(_) | IngestError::Archive(_) | IngestError::Undecided(_) => 503,
    }
}

/// Serve submissions one at a time: the intake owns a SQLite connection, and
/// at one upload per pool per interval the loop is idle between submissions.
///
/// Only returns on error, and every error it can see is terminal. tiny_http
/// 0.12 leaves its accept thread on the first `accept` failure and queues that
/// error once, so a second `recv` would block on an empty queue forever —
/// logging and continuing would leave a process that looks healthy to systemd
/// and accepts nothing. Failing out is what turns it back into a restart.
pub fn serve<A: Store + Bytes, K: Authority>(
    server: &Server,
    intake: &mut Intake<A, K>,
    developer: &Developer,
    page: &str,
) -> Result<std::convert::Infallible, std::io::Error> {
    loop {
        let mut request = server.recv()?;
        let answer = route(intake, developer, page, &mut request);
        respond(request, answer);
    }
}

/// What the server writes back. `claimed` is the pool the headers named, if
/// they got that far: without it a log line cannot say whose uploads are not
/// landing. `headers` carries what a status needs beside the body, which so
/// far is the 401's challenge.
struct Answer {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    claimed: Option<PoolId>,
    headers: Vec<(&'static str, String)>,
}

fn route<A: Store + Bytes, K: Authority>(
    intake: &mut Intake<A, K>,
    developer: &Developer,
    page: &str,
    request: &mut Request,
) -> Answer {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or_default().to_string();
    let method = request.method().clone();
    match path.as_str() {
        // Unauthenticated: it is what an operator reads before they have
        // anything to authenticate with.
        instructions::PATH => match method {
            Method::Get => Answer {
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: page.as_bytes().to_vec(),
                claimed: None,
                headers: Vec::new(),
            },
            _ => refuse(None, 405, format!("{} takes GET", instructions::PATH)),
        },
        // A known route reached with the wrong method is named, because a
        // client that guessed the method is not a client at the wrong address.
        SUBMIT_PATH => match method {
            Method::Post => submit(intake, request),
            _ => refuse(None, 405, format!("{SUBMIT_PATH} takes POST")),
        },
        // Authentication comes before the method check: answering 405 first
        // would confirm the route exists to a client that never presented a
        // credential.
        SUBMISSIONS_PATH | OBJECT_PATH => {
            if let Err(error) = developer.authorize(authorization(request).as_deref()) {
                return challenge(&path, &error);
            }
            if method != Method::Get {
                return refuse(None, 405, format!("{path} takes GET"));
            }
            match path.as_str() {
                SUBMISSIONS_PATH => listing(intake, developer, &url),
                _ => object(intake, &url),
            }
        }
        _ => refuse(
            None,
            404,
            format!("no route {path}, submissions go to {SUBMIT_PATH}"),
        ),
    }
}

fn authorization(request: &Request) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("authorization"))
        .map(|header| header.value.as_str().to_string())
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

/// One page of the index as JSON. Bounded by `list_max_rows`, and the answer
/// says whether the bound cut it off (`index::Listing`).
fn listing<A: Store + Bytes, K: Authority>(
    intake: &Intake<A, K>,
    developer: &Developer,
    url: &str,
) -> Answer {
    let filters = match Filters::parse(url) {
        Ok(filters) => filters,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    match intake
        .index()
        .submissions(&filters.prefix, &filters.after, developer.list_max_rows())
    {
        Ok(listing) => Answer {
            status: 200,
            content_type: "application/json",
            body: developer::page(&listing).into_bytes(),
            claimed: None,
            headers: Vec::new(),
        },
        Err(error) => index_failed(&error),
    }
}

/// How a failed index read is answered. A row `record` never wrote is not
/// something a retry repairs (`index::IndexError::ObjectName`), so it earns a
/// 500 where a database this process cannot reach earns a 503.
fn index_failed(error: &IndexError) -> Answer {
    let status = match error {
        IndexError::ObjectName(_) => 500,
        IndexError::Sqlite(_) | IndexError::Migrate(_) => 503,
    };
    refuse_withholding(
        None,
        status,
        "the index cannot be read".to_string(),
        Some(error.to_string()),
    )
}

/// One object, as the bytes that were archived. Asks `index::Index::holds`
/// before the archive.
fn object<A: Store + Bytes, K: Authority>(intake: &Intake<A, K>, url: &str) -> Answer {
    let key = match developer::query_value(url, KEY_FIELD) {
        Ok(key) => key,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    if key.is_empty() {
        return refuse(None, 400, format!("name the object in ?{KEY_FIELD}="));
    }
    match intake.index().holds(&key) {
        Err(error) => return index_failed(&error),
        Ok(false) => return refuse(None, 404, "no such object".to_string()),
        Ok(true) => (),
    }
    match intake.archive().bytes(&key) {
        Ok(bytes) => Answer {
            status: 200,
            // RFC 8878: the body is the zstd the pool signed, so a developer
            // decompresses it themselves.
            content_type: "application/zstd",
            body: bytes,
            claimed: None,
            headers: Vec::new(),
        },
        Err(error) => unavailable("the archive cannot be read", &error.to_string()),
    }
}

/// A 503 whose body says which store failed and whose log line says how. The
/// detail names the bucket, the endpoint or the database file, and none of
/// that is a client's to read.
fn unavailable(reason: &str, withheld: &str) -> Answer {
    refuse_withholding(None, 503, reason.to_string(), Some(withheld.to_string()))
}

fn submit<A: Store, K: Authority>(intake: &mut Intake<A, K>, request: &mut Request) -> Answer {
    let headers = match SubmissionHeaders::decode(request.headers()) {
        Ok(headers) => headers,
        Err(error) => return refuse(None, 400, error.to_string()),
    };
    let claimed = Some(headers.pool_id);
    let wire_bytes = match read_body(request, intake.max_body_bytes()) {
        Ok(wire_bytes) => wire_bytes,
        Err(reason) => return refuse(claimed, reason.status, reason.text),
    };
    let submission = Signed {
        pool_id: headers.pool_id,
        vkey: headers.vkey,
        signature: headers.signature,
        wire_bytes: &wire_bytes,
    };
    match intake.submit(&submission, OffsetDateTime::now_utc()) {
        Ok(ack) => Answer {
            status: 200,
            content_type: "application/json",
            body: serde_json::to_vec(&ack).expect("an Ack of two strings serializes"),
            claimed,
            headers: Vec::new(),
        },
        Err(error) => refuse_withholding(
            claimed,
            status_for(&error),
            error.to_string(),
            error.withheld(),
        ),
    }
}

/// A body that never reached the intake, with the status it earns.
struct BodyError {
    status: u16,
    text: String,
}

/// Read at most `max_body_bytes`, refusing anything longer. `Content-Length`
/// catches the honest oversized upload before any verification work; the
/// bounded read catches a chunked body that lies about its size. Neither
/// avoids *reading* the excess: tiny_http drains whatever the body declared
/// when the request drops, allocating that much (metsuke-a3a).
fn read_body(request: &mut Request, max_body_bytes: u64) -> Result<Vec<u8>, BodyError> {
    let oversized = |found: usize| BodyError {
        status: 413,
        text: Rejection::OversizedBody {
            found,
            max: max_body_bytes,
        }
        .to_string(),
    };
    if let Some(length) = request.body_length()
        && length as u64 > max_body_bytes
    {
        return Err(oversized(length));
    }
    let mut body = Vec::new();
    request
        .as_reader()
        .take(max_body_bytes.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|error| BodyError {
            status: 400,
            text: format!("could not read the request body: {error}"),
        })?;
    if body.len() as u64 > max_body_bytes {
        return Err(oversized(body.len()));
    }
    Ok(body)
}

/// Every refusal is logged: the reason text is the only record of why a
/// pool's uploads are not landing.
fn refuse(claimed: Option<PoolId>, status: u16, reason: String) -> Answer {
    refuse_withholding(claimed, status, reason, None)
}

/// A refusal whose log line carries more than its body: `withheld` is appended
/// to what is logged and never sent.
fn refuse_withholding(
    claimed: Option<PoolId>,
    status: u16,
    reason: String,
    withheld: Option<String>,
) -> Answer {
    let severity = if status >= 500 { ERR } else { WARNING };
    let logged = match &withheld {
        Some(withheld) => format!("{reason}: {withheld}"),
        None => reason.clone(),
    };
    eprintln!(
        "{severity}refused {}: {status}, {logged}",
        claimant(claimed)
    );
    Answer {
        status,
        content_type: "text/plain; charset=utf-8",
        body: reason.into_bytes(),
        claimed,
        headers: Vec::new(),
    }
}

/// How a request is named before anything it claims has been verified.
fn claimant(claimed: Option<PoolId>) -> String {
    match claimed {
        Some(pool_id) => pool_id.to_bech32(),
        None => "an unidentified client".to_string(),
    }
}

fn respond(request: Request, answer: Answer) {
    let content_type = Header::from_bytes("content-type", answer.content_type)
        .expect("a static content type is a valid header");
    let mut response = Response::from_data(answer.body)
        .with_status_code(answer.status)
        .with_header(content_type);
    for (field, value) in &answer.headers {
        response.add_header(
            Header::from_bytes(field.as_bytes(), value.as_bytes())
                .expect("a header this server writes is well formed"),
        );
    }
    if let Err(error) = request.respond(response) {
        // tiny_http answers `Ok` for a client that merely hung up, so this is
        // a real write failure. On an accepted submission the bytes are
        // already archived and the counter spent, and the agent — never having
        // seen the ack — resends into a replay rejection it cannot act on.
        eprintln!(
            "{ERR}could not answer {} with {}: {error}",
            claimant(answer.claimed),
            answer.status,
        );
    }
}
