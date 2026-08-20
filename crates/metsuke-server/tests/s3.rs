//! The production archive, against an S3 endpoint the test controls. What must
//! hold on the wire: the object is the received body verbatim, its metadata
//! headers are all present and signed, and a PUT the endpoint refuses is an
//! error rather than a silently dropped submission.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use metsuke::envelope::SCHEMA_VERSION;
use metsuke_server::archive::{Fetch, List, ObjectName, Store, StoredSubmission};
use metsuke_server::config::S3Config;
use metsuke_server::rebuild::{EmptyArchive, rebuild};
use metsuke_server::s3::{META_COUNTER, META_SCHEMA_VERSION, META_SIGNATURE, META_VKEY, S3Archive};
use metsuke_server::verify::{audit, verify};
use rusty_s3::Credentials;

mod support;
use support::{
    MAX_DECOMPRESSED_BYTES, counter_store, envelope_for, hex, nonzero_u32, nonzero_u64, pool_of,
    seal, stored_submission, test_key, test_now,
};

/// One request the fake endpoint received.
#[derive(Clone)]
struct Seen {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The object key this request addressed: the path with the bucket and the
    /// presigning query stripped.
    fn object_key(&self) -> &str {
        let path = self.url.split('?').next().unwrap_or_default();
        path.strip_prefix(&format!("/{BUCKET}/")).unwrap_or(path)
    }

    fn lists(&self) -> bool {
        self.url.contains("list-type=2")
    }

    /// This stored request served back as the object it wrote, metadata
    /// headers and all.
    fn as_object(&self) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
        let mut response = tiny_http::Response::from_data(self.body.clone());
        for (field, value) in &self.headers {
            if field.to_lowercase().starts_with("x-amz-meta-") {
                response.add_header(
                    tiny_http::Header::from_bytes(field.as_bytes(), value.as_bytes()).unwrap(),
                );
            }
        }
        response
    }
}

/// An S3 endpoint that records every request and answers a scripted sequence
/// of replies. Once the script runs out it behaves as an object store: a PUT
/// keeps the body and its `x-amz-meta-*` headers, and a GET of that path hands
/// both back. That is what lets a test store an object and read it back
/// without ever asserting on a header name.
struct FakeS3 {
    endpoint: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    server: Arc<tiny_http::Server>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl Drop for FakeS3 {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

impl FakeS3 {
    fn start(replies: Vec<(u16, String)>) -> FakeS3 {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let endpoint = format!("http://{}", server.server_addr());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let serving = std::thread::spawn({
            let server = Arc::clone(&server);
            let seen = Arc::clone(&seen);
            let mut replies = replies.into_iter();
            let mut objects: std::collections::HashMap<String, Seen> =
                std::collections::HashMap::new();
            move || {
                while let Ok(mut request) = server.recv() {
                    let mut body = Vec::new();
                    std::io::Read::read_to_end(request.as_reader(), &mut body).unwrap();
                    let received = Seen {
                        method: request.method().as_str().to_string(),
                        url: request.url().to_string(),
                        headers: request
                            .headers()
                            .iter()
                            .map(|header| {
                                (header.field.to_string(), header.value.as_str().to_string())
                            })
                            .collect(),
                        body,
                    };
                    let key = received.object_key().to_string();
                    seen.lock().unwrap().push(received.clone());
                    let response = match replies.next() {
                        Some((status, body)) => {
                            tiny_http::Response::from_string(body).with_status_code(status)
                        }
                        None if received.method == "PUT" => {
                            objects.insert(key, received);
                            tiny_http::Response::from_string(String::new())
                        }
                        None if received.lists() => {
                            let mut keys: Vec<String> = objects.keys().cloned().collect();
                            keys.sort();
                            tiny_http::Response::from_string(listing(&keys, None))
                        }
                        None => match objects.get(&key) {
                            Some(stored) => stored.as_object(),
                            None => tiny_http::Response::from_string("NoSuchKey".to_string())
                                .with_status_code(404),
                        },
                    };
                    let _ = request.respond(response);
                }
            }
        });
        FakeS3 {
            endpoint,
            seen,
            server,
            serving: Some(serving),
        }
    }

    fn archive(&self, put_retries: u32) -> S3Archive {
        S3Archive::new(&config_for(&self.endpoint, put_retries), credentials()).unwrap()
    }

    /// The same archive with a caller-chosen listing bound, for the tests
    /// about what a listing that will not end does.
    fn archive_listing_at_most(&self, list_max_pages: u32) -> S3Archive {
        let config = S3Config {
            list_max_pages: nonzero_u32(list_max_pages),
            ..config_for(&self.endpoint, 0)
        };
        S3Archive::new(&config, credentials()).unwrap()
    }

    fn requests(&self) -> std::sync::MutexGuard<'_, Vec<Seen>> {
        self.seen.lock().unwrap()
    }
}

fn credentials() -> Credentials {
    Credentials::new("test-key-id", "test-secret")
}

/// Long enough that no test waits on it, short enough that the one test
/// measuring it does not stall the suite.
const TIMEOUT: Duration = Duration::from_secs(1);

/// The bucket every test configures, and the path segment the fake strips to
/// recover an object key.
const BUCKET: &str = "metsuke-test";

/// Short enough that the retry tests do not stall the suite, long enough to be
/// measurable against a PUT that answers immediately.
const RETRY_BACKOFF: Duration = Duration::from_millis(50);

fn config_for(endpoint: &str, put_retries: u32) -> S3Config {
    S3Config {
        bucket: BUCKET.to_string(),
        region: "test-region".to_string(),
        endpoint: endpoint.parse().expect("the fake endpoint is a URL"),
        request_timeout_secs: nonzero_u64(TIMEOUT.as_secs()),
        signature_validity_secs: nonzero_u64(TIMEOUT.as_secs()),
        put_retries,
        put_retry_backoff_ms: nonzero_u64(RETRY_BACKOFF.as_millis() as u64),
        list_max_pages: nonzero_u32(10),
    }
}

/// The counter every test that does not care about the value uses.
const COUNTER: u64 = 42;

/// A submission and the bytes it was sealed from, so a test can compare what
/// the endpoint received against what the client sent.
fn submission(counter: u64) -> (Vec<u8>, metsuke::envelope::Signature) {
    let key = test_key();
    seal(&key, &envelope_for(&key, counter))
}

/// The submission `seal`ed at `counter`, as the archive is asked to store it.
fn stored<'a>(
    counter: u64,
    signature: metsuke::envelope::Signature,
    wire_bytes: &'a [u8],
) -> StoredSubmission<'a> {
    stored_submission(&test_key(), counter, test_now(), signature, wire_bytes)
}

#[test]
fn a_stored_object_is_put_at_its_key_with_the_body_verbatim() {
    let endpoint = FakeS3::start(vec![(200, String::new())]);
    let (wire_bytes, signature) = submission(COUNTER);
    let submission = stored(COUNTER, signature, &wire_bytes);
    endpoint.archive(1).store(&submission).unwrap();

    let requests = endpoint.requests();
    assert_eq!(requests.len(), 1);
    let put = &requests[0];
    assert_eq!(put.method, "PUT");
    assert!(
        put.url
            .starts_with(&format!("/metsuke-test/{}?", submission.object_key())),
        "PUT went to {}",
        put.url
    );
    assert_eq!(put.body, wire_bytes);
}

#[test]
fn the_metadata_headers_carry_what_re_verifying_the_object_needs() {
    let endpoint = FakeS3::start(vec![(200, String::new())]);
    let (wire_bytes, signature) = submission(COUNTER);
    let key = test_key();
    endpoint
        .archive(1)
        .store(&stored(COUNTER, signature, &wire_bytes))
        .unwrap();

    let requests = endpoint.requests();
    let put = &requests[0];
    assert_eq!(
        put.header(META_SIGNATURE),
        Some(hex(&signature.to_bytes()).as_str())
    );
    assert_eq!(
        put.header(META_VKEY),
        Some(hex(key.verifying_key().as_bytes()).as_str())
    );
    assert_eq!(put.header(META_COUNTER), Some(COUNTER.to_string().as_str()));
    assert_eq!(
        put.header(META_SCHEMA_VERSION),
        Some(SCHEMA_VERSION.to_string().as_str())
    );
}

/// The metadata is only trustworthy if it is covered by the request
/// signature; an unsigned header is one a middlebox could rewrite.
#[test]
fn the_metadata_headers_are_signed() {
    let endpoint = FakeS3::start(vec![(200, String::new())]);
    let (wire_bytes, signature) = submission(COUNTER);
    endpoint
        .archive(1)
        .store(&stored(COUNTER, signature, &wire_bytes))
        .unwrap();

    let requests = endpoint.requests();
    let signed = requests[0]
        .url
        .split('&')
        .find(|param| param.starts_with("X-Amz-SignedHeaders="))
        .expect("the URL must be presigned");
    for header in [META_SIGNATURE, META_VKEY, META_COUNTER, META_SCHEMA_VERSION] {
        assert!(signed.contains(header), "{header} is not in {signed}");
    }
}

/// Both halves of the retry: the count, and the `put_retry_backoff_ms` wait
/// that is what makes spending it worth anything.
#[test]
fn a_failed_put_is_retried_up_to_the_configured_count_after_the_backoff() {
    let endpoint = FakeS3::start(vec![(500, "slow down".to_string()), (200, String::new())]);
    let (wire_bytes, signature) = submission(COUNTER);
    let started = std::time::Instant::now();
    endpoint
        .archive(1)
        .store(&stored(COUNTER, signature, &wire_bytes))
        .unwrap();
    assert_eq!(endpoint.requests().len(), 2);
    assert!(
        started.elapsed() >= RETRY_BACKOFF,
        "the retry came after {:?}, not the configured {RETRY_BACKOFF:?}",
        started.elapsed()
    );
}

#[test]
fn a_put_that_keeps_failing_is_an_error_naming_the_key_and_the_attempts() {
    let endpoint = FakeS3::start(vec![(500, "no".to_string()), (500, "no".to_string())]);
    let (wire_bytes, signature) = submission(COUNTER);
    let submission = stored(COUNTER, signature, &wire_bytes);
    let error = endpoint
        .archive(1)
        .store(&submission)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(&submission.object_key()) && error.contains("2 attempts"),
        "got: {error}"
    );
    assert_eq!(endpoint.requests().len(), 2);
}

/// A rejected PUT will not start working on a retry, and the archive is on the
/// path of a request the client is waiting on.
#[test]
fn a_refused_put_is_not_retried() {
    let endpoint = FakeS3::start(vec![(403, "AccessDenied".to_string())]);
    let (wire_bytes, signature) = submission(COUNTER);
    let error = endpoint
        .archive(1)
        .store(&stored(COUNTER, signature, &wire_bytes))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("403") && error.contains("1 attempt"),
        "got: {error}"
    );
    assert_eq!(endpoint.requests().len(), 1);
}

/// The ADR-0005 round trip, with no assertion about header names in it: what
/// went into the bucket comes back out and verifies from itself alone.
#[test]
fn a_stored_object_fetches_back_and_verifies() {
    let endpoint = FakeS3::start(Vec::new());
    let (wire_bytes, signature) = submission(COUNTER);
    let submission = stored(COUNTER, signature, &wire_bytes);
    let archive = endpoint.archive(1);
    archive.store(&submission).unwrap();

    let fetched = archive.fetch(&submission.object_key()).unwrap();
    assert_eq!(fetched.wire_bytes, wire_bytes);
    let envelope = verify(&fetched, MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(envelope.counter, submission.counter);
    assert_eq!(envelope.pool_id, pool_of(&test_key()));
}

/// What an operator runs over the whole bucket: every object fetched and
/// re-verified, and anything that did not named.
#[test]
fn an_audit_verifies_what_is_stored_and_names_what_is_missing() {
    let endpoint = FakeS3::start(Vec::new());
    let archive = endpoint.archive(1);
    let key = test_key();
    let stored_keys: Vec<String> = [1u64, 2]
        .into_iter()
        .map(|counter| {
            let envelope = envelope_for(&key, counter);
            let (wire_bytes, signature) = seal(&key, &envelope);
            let submission =
                stored_submission(&key, counter, envelope.timestamp, signature, &wire_bytes);
            archive.store(&submission).unwrap();
            submission.object_key()
        })
        .collect();

    let found = audit(&archive, MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(found.verified, 2);
    assert!(found.failures.is_empty(), "{:?}", found.failures);

    // A listing naming objects a bucket will not hand back: the audit reports
    // each one rather than counting what it could not read as clean.
    let missing = ObjectName {
        pool_id: pool_of(&key),
        counter: 3,
        timestamp: test_now(),
    };
    let mut listed = stored_keys.clone();
    listed.push(missing.to_key());
    let endpoint = FakeS3::start(vec![(200, listing(&listed, None))]);
    let found = audit(&endpoint.archive(1), MAX_DECOMPRESSED_BYTES).unwrap();
    assert_eq!(found.verified, 0);
    // Unreadable, not failed: nothing was checked, so nothing can be said
    // about these objects.
    assert_eq!(found.unreadable(), 3, "{:?}", found.failures);
    assert_eq!(found.failed(), 0);
}

#[test]
fn fetching_a_key_the_bucket_does_not_hold_is_an_error() {
    let endpoint = FakeS3::start(Vec::new());
    let key = stored(COUNTER, submission(COUNTER).1, b"").object_key();
    let error = endpoint.archive(1).fetch(&key).unwrap_err().to_string();
    assert!(
        error.contains(&key) && error.contains("404"),
        "got: {error}"
    );
}

#[test]
fn fetching_an_object_without_its_metadata_names_the_missing_header() {
    let endpoint = FakeS3::start(vec![(200, "body".to_string())]);
    let key = stored(COUNTER, submission(COUNTER).1, b"").object_key();
    let error = endpoint.archive(1).fetch(&key).unwrap_err().to_string();
    assert!(error.contains(META_VKEY), "got: {error}");
}

/// A URL the config accepts but a bucket cannot be built on: `S3Config` makes
/// the unparseable case unrepresentable, and this is what is left for
/// `S3Error::Endpoint` to catch at startup.
#[test]
fn a_url_that_is_not_an_s3_endpoint_is_refused_at_construction() {
    let error = S3Archive::new(&config_for("mailto:nobody@example.org", 0), credentials())
        .unwrap_err()
        .to_string();
    assert!(error.contains("mailto:nobody@example.org"), "got: {error}");
}

#[test]
fn an_endpoint_that_is_not_listening_is_an_error() {
    // Nothing listens on discard/9, so this fails without waiting out the
    // timeout.
    let archive = S3Archive::new(&config_for("http://127.0.0.1:9", 0), credentials()).unwrap();
    let (wire_bytes, signature) = submission(COUNTER);
    let submission = stored(COUNTER, signature, &wire_bytes);
    let error = archive.store(&submission).unwrap_err().to_string();
    assert!(error.contains(&submission.object_key()), "got: {error}");
}

/// The two objects a listing fixture holds, keyed the way the archive keys
/// them, so what the listing returns is what `ObjectName` can read back.
fn listed_keys() -> Vec<String> {
    [1u64, 2]
        .into_iter()
        .map(|counter| {
            ObjectName {
                pool_id: pool_of(&test_key()),
                counter,
                timestamp: test_now() + time::Duration::seconds(counter as i64),
            }
            .to_key()
        })
        .collect()
}

/// A `ListObjectsV2` answer, keys percent-encoded because the request asks for
/// `encoding-type=url` (asserted in
/// `listing_follows_the_continuation_token_to_the_end`).
fn listing(keys: &[String], next: Option<&str>) -> String {
    let contents: String = keys
        .iter()
        .map(|key| {
            format!(
                "    <Contents>
        <Key>{key}</Key>
        <LastModified>2025-08-12T14:00:00.000Z</LastModified>
        <ETag>\"e\"</ETag>
        <Size>4</Size>
    </Contents>\n",
                key = key.replace('/', "%2F"),
            )
        })
        .collect();
    let truncated = match next {
        Some(token) => format!(
            "    <IsTruncated>true</IsTruncated>
    <NextContinuationToken>{token}</NextContinuationToken>\n"
        ),
        None => "    <IsTruncated>false</IsTruncated>\n".to_string(),
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
    <Name>metsuke-test</Name>
{truncated}{contents}    <EncodingType>url</EncodingType>
</ListBucketResult>"#
    )
}

#[test]
fn listing_follows_the_continuation_token_to_the_end() {
    let listed = listed_keys();
    let endpoint = FakeS3::start(vec![
        (200, listing(&listed[..1], Some("page-two"))),
        (200, listing(&listed[1..], None)),
    ]);
    assert_eq!(endpoint.archive(1).keys().unwrap(), listed);
    let requests = endpoint.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].method == "GET" && !requests[0].url.contains("continuation-token"));
    // What makes the percent-encoded keys in the answer the encoding to expect.
    assert!(
        requests[0].url.contains("encoding-type=url"),
        "got {}",
        requests[0].url
    );
    assert!(
        requests[1].url.contains("continuation-token=page-two"),
        "the second page must be asked for by token, got {}",
        requests[1].url
    );
}

/// The seam ADR 0005 rests on: keys as S3 hands them back are keys the rebuild
/// can seed counters from, with no filesystem in between.
#[test]
fn a_bucket_listing_seeds_the_rebuild() {
    let listed = listed_keys();
    let endpoint = FakeS3::start(vec![(200, listing(&listed, None))]);
    let dir = tempfile::tempdir().unwrap();
    let mut counters = counter_store(dir.path());
    let summary = rebuild(&endpoint.archive(1), &mut counters, EmptyArchive::Refuse).unwrap();

    assert_eq!(summary.objects, listed.len());
    let highest = listed
        .iter()
        .map(|key| ObjectName::parse(key).unwrap().counter)
        .max();
    assert_eq!(
        counters.last_counter(pool_of(&test_key())).unwrap(),
        highest
    );
}

/// An endpoint that stays truncated forever, stopped by `list_max_pages`.
#[test]
fn a_listing_that_never_ends_fails_at_the_configured_page_bound() {
    let listed = listed_keys();
    let endpoint = FakeS3::start(
        (0..8)
            .map(|page| (200, listing(&listed[..1], Some(&format!("page-{page}")))))
            .collect(),
    );
    let error = endpoint
        .archive_listing_at_most(3)
        .keys()
        .unwrap_err()
        .to_string();
    assert!(error.contains("3 pages"), "got: {error}");
    assert_eq!(endpoint.requests().len(), 3);
}

/// The same non-ending listing by another route: a token that repeats.
#[test]
fn a_listing_whose_token_does_not_advance_is_an_error() {
    let listed = listed_keys();
    let endpoint = FakeS3::start(vec![
        (200, listing(&listed[..1], Some("stuck"))),
        (200, listing(&listed[..1], Some("stuck"))),
    ]);
    let error = endpoint
        .archive_listing_at_most(100)
        .keys()
        .unwrap_err()
        .to_string();
    assert!(error.contains("stuck"), "got: {error}");
    assert_eq!(endpoint.requests().len(), 2);
}

#[test]
fn a_listing_the_endpoint_refuses_is_an_error() {
    let endpoint = FakeS3::start(vec![(500, "NoSuchBucket".to_string())]);
    let error = endpoint.archive(1).keys().unwrap_err().to_string();
    assert!(error.contains("500"), "got: {error}");
}

#[test]
fn a_listing_that_is_not_the_expected_xml_is_an_error() {
    let endpoint = FakeS3::start(vec![(200, "<html>hello</html>".to_string())]);
    assert!(endpoint.archive(1).keys().is_err());
}

/// The timeout bounds the whole request, so an endpoint that accepts and then
/// says nothing cannot hold the ingest thread indefinitely.
#[test]
fn a_put_gives_up_at_the_configured_timeout() {
    let stalling = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", stalling.server_addr());
    // Hold the request unanswered for as long as the test needs it: dropping
    // it would make tiny_http answer 500 and the PUT would fail at once,
    // which is what the timeout must be distinguished from.
    let (release, released) = std::sync::mpsc::channel::<()>();
    let stalled = std::thread::spawn(move || {
        let held = stalling.recv().unwrap();
        let _ = released.recv();
        drop(held);
    });
    let archive = S3Archive::new(&config_for(&endpoint, 0), credentials()).unwrap();
    let (wire_bytes, signature) = submission(COUNTER);
    let started = std::time::Instant::now();
    assert!(
        archive
            .store(&stored(COUNTER, signature, &wire_bytes))
            .is_err()
    );
    let elapsed = started.elapsed();
    assert!(
        elapsed >= TIMEOUT && elapsed < TIMEOUT * 10,
        "the PUT took {elapsed:?}, not the configured {TIMEOUT:?}"
    );
    drop(release);
    stalled.join().unwrap();
}
