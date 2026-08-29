//! The answer every route gives, as a value. `http::answer` takes a decoded
//! request and returns a status, a body and the headers that go with it, so
//! the whole surface is reachable here, and only what a socket adds (timeouts,
//! the body cap, streaming) is left to `tests/binary.rs`.

use metsuke_server::archive::FilesystemArchive;
use metsuke_server::developer::Developer;
use metsuke_server::http::{
    self, Answer, AnswerBody, HeaderError, KEY_FIELD, Method, OBJECT_PATH, Request,
    SUBMISSIONS_PATH, SUBMIT_PATH, SubmissionHeaders, UNAUTHORIZED_BODY,
};
use metsuke_server::instructions;
use metsuke_server::intake::Intake;
use metsuke_wire::envelope::{HEADER_SIGNATURE, HEADER_VKEY, SigningKey};

mod support;
use base64::Engine as _;
use metsuke_wire::hex;
use support::{
    DEVELOPER_PASSWORD, FailingArchive, developer_config, envelope_for, other_key,
    permissive_config, pool_of, seal, test_key,
};

/// The server one request is answered by: an archive under a temporary
/// directory, the suite's developer account, and the shipped page.
struct Server<A: metsuke_server::archive::Store> {
    intake: Intake<A>,
    developer: Developer,
    page: bytes::Bytes,
    _dir: tempfile::TempDir,
}

impl<A> Server<A>
where
    A: metsuke_server::archive::Store
        + metsuke_server::archive::Bytes
        + metsuke_server::archive::List,
{
    fn answer(&self, request: Request) -> Answer {
        http::answer(&self.intake, &self.developer, &self.page, request)
    }
}

fn server() -> Server<FilesystemArchive> {
    let dir = tempfile::tempdir().unwrap();
    let archive = FilesystemArchive::new(&dir.path().join("archive"));
    over(archive, dir)
}

/// Why `unreachable_archive` fails, and so exactly what `withholds` asserts is
/// absent: one string, or the assertion could outlive the fixture wording and
/// pass against a body that never carried it.
const UNREACHABLE_REASON: &str = "the bucket at s3.example is unreachable";

/// The same server over an archive that fails whichever half is used.
fn unreachable_archive() -> Server<FailingArchive> {
    let dir = tempfile::tempdir().unwrap();
    over(
        FailingArchive {
            reason: UNREACHABLE_REASON,
        },
        dir,
    )
}

fn over<A: metsuke_server::archive::Store>(archive: A, dir: tempfile::TempDir) -> Server<A> {
    let developer = Developer::new(&developer_config(dir.path()), DEVELOPER_PASSWORD);
    Server {
        intake: Intake::new(permissive_config(&[pool_of(&test_key())]), archive),
        developer,
        page: bytes::Bytes::from(instructions::page()),
        _dir: dir,
    }
}

fn get(target: &str) -> Request {
    Request {
        method: Method::Get,
        target: target.to_string(),
        submission: SubmissionHeaders::decode(None, None),
        authorization: None,
        body: Vec::new(),
    }
}

/// A GET the configured developer account is authorized to make.
fn pull(target: &str) -> Request {
    let user = developer_config(std::path::Path::new("/tmp")).user;
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{DEVELOPER_PASSWORD}"));
    Request {
        authorization: Some(format!("Basic {encoded}")),
        ..get(target)
    }
}

/// A sealed submission with the two ADR-0001 headers a well-formed upload
/// carries.
fn post(key: &SigningKey, counter: u64) -> Request {
    let (body, signature) = seal(key, &envelope_for(key, counter));
    Request {
        method: Method::Post,
        target: SUBMIT_PATH.to_string(),
        submission: SubmissionHeaders::decode(
            Some(&hex::encode(key.verifying_key().as_bytes())),
            Some(&hex::encode(&signature.to_bytes())),
        ),
        authorization: None,
        body,
    }
}

/// The answer's body where it is bytes, which is every answer but a download.
fn body(answer: &Answer) -> String {
    let AnswerBody::Bytes(bytes) = &answer.body else {
        panic!("this answer's body is a stream, not bytes in hand");
    };
    String::from_utf8_lossy(bytes).to_string()
}

/// What every use of the archive answers when the store will not (`http`,
/// `unavailable`): a 503 the client may retry, whose body does not carry the
/// reason the store gave.
fn withholds(answer: &Answer) {
    assert_eq!(answer.status, 503, "{}", body(answer));
    assert!(
        !body(answer).contains(UNREACHABLE_REASON),
        "the store's own error is the operator's to see, not the client's: {}",
        body(answer)
    );
}

#[test]
fn the_instructions_page_is_answered_without_credentials() {
    let server = server();

    let answer = server.answer(get(instructions::PATH));

    assert_eq!(answer.status, 200);
    assert_eq!(body(&answer), instructions::page());
    assert!(answer.content_type.starts_with("text/html"));
}

#[test]
fn the_instructions_page_takes_only_get() {
    let answer = server().answer(Request {
        method: Method::Post,
        ..get(instructions::PATH)
    });

    assert_eq!(answer.status, 405);
}

#[test]
fn a_route_nothing_serves_names_where_submissions_go() {
    let answer = server().answer(get("/nowhere"));

    assert_eq!(answer.status, 404);
    assert!(
        body(&answer).contains(SUBMIT_PATH),
        "got: {}",
        body(&answer)
    );
}

#[test]
fn the_submit_route_takes_only_post() {
    let answer = server().answer(get(SUBMIT_PATH));

    assert_eq!(answer.status, 405);
    assert!(body(&answer).contains("POST"), "got: {}", body(&answer));
}

#[test]
fn a_sealed_submission_from_an_allowlisted_pool_is_acked() {
    let server = server();

    let answer = server.answer(post(&test_key(), 1));

    assert_eq!(answer.status, 200, "{}", body(&answer));
    let ack: metsuke_wire::envelope::Ack = serde_json::from_str(&body(&answer)).unwrap();
    assert_eq!(ack.latest_version, metsuke_server::CLIENT_VERSION);
}

/// The 4xx an agent reads as permanent, each from the check that produced it
/// (`http::status_for`).
#[test]
fn a_pool_off_the_allowlist_is_forbidden() {
    let stranger = other_key();

    let answer = server().answer(post(&stranger, 1));

    assert_eq!(answer.status, 403, "{}", body(&answer));
    assert!(
        body(&answer).contains("allowlist"),
        "got: {}",
        body(&answer)
    );
    // The pool the presented key derives to, named in the refusal: it is
    // derived before anything admits it, so a stranger is told which identity
    // was turned away.
    assert!(
        body(&answer).contains(&pool_of(&stranger).to_bech32()),
        "got: {}",
        body(&answer)
    );
}

/// An archive that will not take the bytes is a 5xx, or the agent acks and
/// deletes scrapes that were never stored (ADR 0004). Reached by an
/// allowlisted pool, which is what makes the withholding matter (`http`,
/// `unavailable`).
#[test]
fn an_archive_that_cannot_store_is_unavailable() {
    withholds(&unreachable_archive().answer(post(&test_key(), 1)));
}

#[test]
fn a_submission_without_the_headers_names_the_missing_one() {
    let answer = server().answer(Request {
        submission: SubmissionHeaders::decode(None, None),
        ..post(&test_key(), 1)
    });

    assert_eq!(answer.status, 400);
    assert!(
        body(&answer).contains(HEADER_VKEY),
        "got: {}",
        body(&answer)
    );
}

/// Both developer routes are shut to a client that presents nothing, and the
/// 401 carries the challenge `curl -u` and a browser prompt need.
#[test]
fn both_developer_routes_refuse_a_request_without_credentials() {
    let server = server();
    for target in [
        SUBMISSIONS_PATH.to_string(),
        format!("{OBJECT_PATH}?{KEY_FIELD}=anything"),
    ] {
        let answer = server.answer(get(&target));

        assert_eq!(answer.status, 401, "{target}");
        assert_eq!(
            body(&answer),
            UNAUTHORIZED_BODY,
            "{target} told the client why it was refused"
        );
        let challenge = answer
            .headers
            .iter()
            .find(|(field, _)| *field == "www-authenticate")
            .unwrap_or_else(|| panic!("{target} answered no challenge"));
        assert!(challenge.1.contains("Basic"), "got: {challenge:?}");
    }
}

/// The method check must not answer before the credential one: a 405 to an
/// unauthenticated client confirms the route exists.
#[test]
fn a_wrong_method_on_a_developer_route_is_refused_as_unauthenticated() {
    let answer = server().answer(Request {
        method: Method::Post,
        ..get(SUBMISSIONS_PATH)
    });

    assert_eq!(answer.status, 401);
    assert!(
        !body(&answer).contains("GET"),
        "a 401 must not say which method the route takes"
    );
}

#[test]
fn an_authorized_listing_answers_the_archive() {
    let server = server();
    assert_eq!(server.answer(post(&test_key(), 1)).status, 200);

    let answer = server.answer(pull(SUBMISSIONS_PATH));

    assert_eq!(answer.status, 200, "{}", body(&answer));
    let page: serde_json::Value = serde_json::from_str(&body(&answer)).unwrap();
    assert_eq!(page["keys"].as_array().unwrap().len(), 1);
    assert_eq!(page["truncated"], false);
}

#[test]
fn a_listing_over_an_archive_that_will_not_answer_is_unavailable() {
    withholds(&unreachable_archive().answer(pull(SUBMISSIONS_PATH)));
}

/// The download route, which is the one that reaches an endpoint by name
/// (`ArchiveError::EndpointUnusable`).
#[test]
fn a_download_from_an_archive_that_will_not_answer_is_unavailable() {
    withholds(
        &unreachable_archive().answer(pull(&format!("{OBJECT_PATH}?{KEY_FIELD}=any-object"))),
    );
}

/// A download names its object in a query field, and the mistake of naming
/// none is worth its own message: an empty key would otherwise read as "no
/// such object".
#[test]
fn a_download_without_a_key_names_the_field() {
    let answer = server().answer(pull(OBJECT_PATH));

    assert_eq!(answer.status, 400);
    assert!(body(&answer).contains(KEY_FIELD), "got: {}", body(&answer));
}

#[test]
fn a_download_of_an_object_the_archive_does_not_hold_is_not_found() {
    let server = server();
    assert_eq!(server.answer(post(&test_key(), 1)).status, 200);
    let stored = stored_key(&server);
    // The same shape, one character off, so what refuses it is the archive and
    // not the key parser.
    let missing = stored.replace("-metrics.jsonl.zst", "-logs.jsonl.zst");

    let answer = server.answer(pull(&format!("{OBJECT_PATH}?{KEY_FIELD}={missing}")));

    assert_eq!(answer.status, 404, "{}", body(&answer));
}

/// A filesystem archive that answers the metadata an S3 one holds beside an
/// object, so the download route's half of the check a consumer runs is
/// reachable without a bucket.
struct Attested {
    inner: FilesystemArchive,
    attestation: metsuke_server::archive::Attestation,
}

impl metsuke_server::archive::Store for Attested {
    fn store(
        &self,
        submission: &metsuke_server::archive::StoredSubmission<'_>,
    ) -> Result<(), metsuke_server::archive::ArchiveError> {
        self.inner.store(submission)
    }
}

impl metsuke_server::archive::Bytes for Attested {
    fn reader(
        &self,
        key: &str,
    ) -> Result<metsuke_server::archive::ObjectStream, metsuke_server::archive::ArchiveError> {
        Ok(metsuke_server::archive::ObjectStream {
            attestation: Some(self.attestation),
            ..self.inner.reader(key)?
        })
    }
}

impl metsuke_server::archive::List for Attested {
    fn location(&self) -> String {
        self.inner.location()
    }

    fn for_each_key<E: From<metsuke_server::archive::ArchiveError>>(
        &self,
        visit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        self.inner.for_each_key(visit)
    }

    fn page(
        &self,
        prefix: &str,
        after: &str,
        max_keys: std::num::NonZeroU32,
    ) -> Result<metsuke_server::archive::Page, metsuke_server::archive::ArchiveError> {
        self.inner.page(prefix, after, max_keys)
    }
}

/// What a consumer checks the bytes with travels with them: the same two
/// headers the pool sent, so a download is verifiable without asking the
/// server to be believed about anything.
#[test]
fn a_download_carries_the_key_and_signature_the_pool_sent() {
    let key = test_key();
    let (_, signature) = seal(&key, &envelope_for(&key, 4));
    let dir = tempfile::tempdir().unwrap();
    let server = over(
        Attested {
            inner: FilesystemArchive::new(&dir.path().join("archive")),
            attestation: metsuke_server::archive::Attestation {
                vkey: key.verifying_key(),
                signature,
            },
        },
        dir,
    );
    assert_eq!(server.answer(post(&key, 4)).status, 200);

    let answer = server.answer(pull(&format!(
        "{OBJECT_PATH}?{KEY_FIELD}={}",
        stored_key(&server)
    )));

    let sent: std::collections::HashMap<&str, String> = answer.headers.into_iter().collect();
    assert_eq!(
        sent.get(HEADER_VKEY),
        Some(&hex::encode(key.verifying_key().as_bytes()))
    );
    assert_eq!(
        sent.get(HEADER_SIGNATURE),
        Some(&hex::encode(&signature.to_bytes()))
    );
}

/// And an archive that holds no metadata says so by sending none, rather than
/// by withholding the bytes: what to do about an object it cannot check is the
/// consumer's to decide.
#[test]
fn a_download_from_an_archive_without_metadata_carries_no_headers() {
    let server = server();
    let key = test_key();
    assert_eq!(server.answer(post(&key, 4)).status, 200);

    let answer = server.answer(pull(&format!(
        "{OBJECT_PATH}?{KEY_FIELD}={}",
        stored_key(&server)
    )));

    assert_eq!(answer.status, 200);
    assert!(answer.headers.is_empty(), "got: {:?}", answer.headers);
}

/// The download hands back the archive's reader (metsuke-4zo.72).
#[test]
fn a_download_answers_the_stored_bytes_as_a_stream() {
    use std::io::Read as _;

    let server = server();
    let key = test_key();
    let (wire_bytes, _) = seal(&key, &envelope_for(&key, 4));
    let answer = server.answer(post(&key, 4));
    assert_eq!(answer.status, 200, "{}", body(&answer));

    let answer = server.answer(pull(&format!(
        "{OBJECT_PATH}?{KEY_FIELD}={}",
        stored_key(&server)
    )));

    assert_eq!(answer.status, 200);
    assert_eq!(answer.content_type, "application/zstd");
    let AnswerBody::Stream(mut stream) = answer.body else {
        panic!("a download must not be buffered");
    };
    assert_eq!(stream.length, wire_bytes.len() as u64);
    let mut downloaded = Vec::new();
    stream.reader.read_to_end(&mut downloaded).unwrap();
    assert_eq!(
        downloaded, wire_bytes,
        "a developer verifies the signature over exactly these bytes"
    );
}

/// The key of the one object the archive holds, read off the listing because
/// the id in it is the server's, stamped at receipt.
fn stored_key<A>(server: &Server<A>) -> String
where
    A: metsuke_server::archive::Store
        + metsuke_server::archive::Bytes
        + metsuke_server::archive::List,
{
    let answer = server.answer(pull(SUBMISSIONS_PATH));
    let page: serde_json::Value = serde_json::from_str(&body(&answer)).unwrap();
    match page["keys"].as_array().unwrap().as_slice() {
        [key] => key.as_str().unwrap().to_string(),
        other => panic!("expected one stored object, got {other:?}"),
    }
}

/// Header decode: the two ADR-0001 headers are the only thing standing between
/// an arbitrary internet request and the intake, so every malformed shape must
/// name what is wrong rather than reach `submit`.
mod headers {
    use super::*;

    /// The pair a well-formed upload from `key` carries.
    fn presented(key: &SigningKey) -> (String, String) {
        let (_, signature) = seal(key, &envelope_for(key, 1));
        (
            hex::encode(key.verifying_key().as_bytes()),
            hex::encode(&signature.to_bytes()),
        )
    }

    /// Decode with one header's value replaced, keeping the other well formed.
    fn with(field: &str, value: Option<String>) -> Result<SubmissionHeaders, HeaderError> {
        let (vkey, signature) = presented(&test_key());
        match field == HEADER_VKEY {
            true => SubmissionHeaders::decode(value.as_deref(), Some(&signature)),
            false => SubmissionHeaders::decode(Some(&vkey), value.as_deref()),
        }
    }

    #[test]
    fn valid_headers_decode_to_the_presented_identity() {
        let key = test_key();
        let (wire_bytes, _) = seal(&key, &envelope_for(&key, 1));
        let (vkey, signature) = presented(&key);

        let decoded = SubmissionHeaders::decode(Some(&vkey), Some(&signature)).unwrap();

        assert_eq!(
            decoded.pool_id(),
            pool_of(&key),
            "the pool is derived from the key, not sent beside it"
        );
        assert_eq!(decoded.vkey, key.verifying_key());
        // Decoded well enough to verify with: the whole point of the layer.
        assert!(
            decoded
                .vkey
                .verify_strict(&wire_bytes, &decoded.signature)
                .is_ok()
        );
    }

    #[test]
    fn a_missing_header_names_it() {
        for field in [HEADER_VKEY, HEADER_SIGNATURE] {
            let error = with(field, None).unwrap_err();
            assert!(
                error.to_string().contains(field),
                "{field} missing must be named, got: {error}"
            );
        }
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        let error = with(HEADER_VKEY, Some("ab".repeat(31))).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains(HEADER_VKEY) && text.contains("31"),
            "got: {text}"
        );
    }

    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let error = with(HEADER_SIGNATURE, Some("ab".repeat(65))).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains(HEADER_SIGNATURE) && text.contains("65"),
            "got: {text}"
        );
    }

    #[test]
    fn a_non_hex_key_is_refused() {
        for value in ["zz".repeat(32), "abc".to_string()] {
            let error = with(HEADER_VKEY, Some(value.clone())).unwrap_err();
            assert!(
                error.to_string().contains(HEADER_VKEY),
                "{value:?} must be refused naming the header, got: {error}"
            );
        }
    }

    #[test]
    fn uppercase_hex_decodes() {
        let key = test_key();
        let uppercase = hex::encode(key.verifying_key().as_bytes()).to_uppercase();

        let decoded = with(HEADER_VKEY, Some(uppercase)).unwrap();

        assert_eq!(decoded.vkey, key.verifying_key());
    }

    /// Thirty-two bytes of hex whose y coordinate is on no curve point: the one
    /// malformed key shape that survives length and hex checks.
    #[test]
    fn a_key_that_is_not_a_curve_point_is_refused() {
        let not_a_point = format!("02{}", "00".repeat(31));

        let error = with(HEADER_VKEY, Some(not_a_point)).unwrap_err();

        assert!(error.to_string().contains(HEADER_VKEY), "got: {error}");
    }
}
