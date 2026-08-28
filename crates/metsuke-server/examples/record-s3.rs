//! Records the S3 cassette the archive suite replays. Run it through
//! scripts/record-s3-fixtures.sh, which brings the endpoint up and passes the
//! environment read below; what the files hold is tests/fixtures/README.md.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use metsuke_server::archive::{Bytes, Fetch, KEY_PREFIX, Kind, List, Store, StoredSubmission};
use metsuke_server::config::S3Config;
use metsuke_server::s3::S3Archive;
use rusty_s3::actions::{ListObjectsV2, S3Action as _};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use time::OffsetDateTime;

// The suite's own helpers, so the key, the agent id and the envelope a
// recording is made from are the ones the tests stamp with, and the cassette
// is written by whatever reads it.
#[path = "../tests/support/mod.rs"]
mod support;
use support::{Reply, envelope_at, nonzero_u32, nonzero_u64, object_name, seal, test_key};

/// Forwards to the endpoint and keeps every answer. The upstream sees the
/// proxy's own `Host`, which is the host the presigned URL was signed for.
struct Recorder {
    endpoint: String,
    seen: Arc<Mutex<Vec<(String, Reply)>>>,
    _serving: std::thread::JoinHandle<()>,
}

/// The same request with the headers the caller sent on it.
fn carrying<T>(
    mut request: ureq::RequestBuilder<T>,
    headers: &[(String, String)],
) -> ureq::RequestBuilder<T> {
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request
}

impl Recorder {
    fn start(upstream: &str) -> Recorder {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", server.server_addr());
        let seen: Arc<Mutex<Vec<(String, Reply)>>> = Arc::new(Mutex::new(Vec::new()));
        let serving = std::thread::spawn({
            let upstream = upstream.to_string();
            let seen = Arc::clone(&seen);
            let agent = metsuke_wire::http::agent(Duration::from_secs(30));
            move || {
                for mut request in server.incoming_requests() {
                    let mut body = Vec::new();
                    request.as_reader().read_to_end(&mut body).unwrap();
                    let line = format!("{} {}", request.method(), request.url());
                    let url = format!("{upstream}{}", request.url());
                    // Every header the client sent, `host` included: the
                    // presigned signature covers them, so one dropped here is
                    // a request the endpoint refuses.
                    let sent: Vec<(String, String)> = request
                        .headers()
                        .iter()
                        .filter(|header| !header.field.equiv("content-length"))
                        .map(|header| (header.field.to_string(), header.value.as_str().to_string()))
                        .collect();
                    let mut answer = match request.method().as_str() {
                        "PUT" => carrying(agent.put(&url), &sent).send(&body),
                        "GET" => carrying(agent.get(&url), &sent).call(),
                        method => panic!("the archive sent a {method}"),
                    }
                    .expect("the endpoint answered");
                    let reply = Reply {
                        status: answer.status().as_u16(),
                        headers: answer
                            .headers()
                            .iter()
                            .map(|(name, value)| {
                                (
                                    name.to_string(),
                                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                                )
                            })
                            .collect(),
                        body: answer.body_mut().read_to_vec().expect("a readable answer"),
                    };
                    let response = reply.as_response();
                    seen.lock().unwrap().push((line, reply));
                    let _ = request.respond(response);
                }
            }
        });
        Recorder {
            endpoint,
            seen,
            _serving: serving,
        }
    }

    /// The answer to the call just made, written as `<name>.http`. Panics
    /// where the call made none, or made more than one: a cassette entry that
    /// recorded something else is the bug this script exists to avoid.
    fn record(&self, name: &str, provenance: &str) {
        let mut seen = self.seen.lock().unwrap();
        let [(request, reply)] = &seen[..] else {
            panic!("{name}: {} requests to record, not one", seen.len());
        };
        let path = reply
            .write(&support::cassette(), name, request, provenance)
            .unwrap_or_else(|error| panic!("writing {name}: {error}"));
        println!("recorded: {} ({})", path.display(), reply.status);
        seen.clear();
    }

    fn forget(&self) {
        self.seen.lock().unwrap().clear();
    }

    fn bucket(&self, name: &str, region: &str) -> Bucket {
        // Path style, as `S3Archive` addresses a bucket.
        Bucket::new(
            self.endpoint.parse().expect("the proxy is a URL"),
            UrlStyle::Path,
            name.to_string(),
            region.to_string(),
        )
        .expect("the proxy is an endpoint")
    }
}

fn required(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is not set; run scripts/record-s3-fixtures.sh"))
}

/// How long a presigned URL stays usable. Long enough for one recording run.
const VALIDITY: Duration = Duration::from_secs(300);

fn config_for(endpoint: &str, bucket: &str, region: &str) -> S3Config {
    S3Config {
        bucket: bucket.to_string(),
        region: region.to_string(),
        endpoint: endpoint.parse().expect("the recorder's endpoint is a URL"),
        request_timeout_ms: nonzero_u64(30_000),
        signature_validity_secs: nonzero_u64(VALIDITY.as_secs()),
        put_retries: 0,
        put_retry_backoff_ms: nonzero_u64(100),
        list_max_pages: nonzero_u32(10),
    }
}
/// One `ListObjectsV2`, bounded to a key and resumed the way
/// `List::for_each_key` resumes, by the token the last answer handed back.
/// Sent from here rather than through the archive because the walk there is
/// unbounded, and an unbounded page against this bucket is one answer.
fn list_page(
    bucket: &Bucket,
    credentials: &Credentials,
    agent: &ureq::Agent,
    token: Option<String>,
) -> Option<String> {
    let mut action = bucket.list_objects_v2(Some(credentials));
    action.with_prefix(KEY_PREFIX);
    action.with_max_keys(1);
    if let Some(token) = token {
        action.with_continuation_token(token);
    }
    let url = action.sign(VALIDITY);
    let answer = agent
        .get(url.as_str())
        .call()
        .expect("the bucket lists")
        .body_mut()
        .read_to_string()
        .expect("a listing is text");
    ListObjectsV2::parse_response(&answer)
        .expect("a listing parses")
        .next_continuation_token
}

fn main() {
    let provenance = format!(
        "recorded from {} by scripts/record-s3-fixtures.sh",
        required("ENDPOINT_VERSION")
    );
    let bucket = required("BUCKET");
    let region = required("REGION");
    let credentials = Credentials::new(required("ACCESS_KEY_ID"), required("SECRET_ACCESS_KEY"));

    let recorder = Recorder::start(&required("ENDPOINT"));
    let agent = metsuke_wire::http::agent(Duration::from_secs(30));
    let archive = S3Archive::new(
        &config_for(&recorder.endpoint, &bucket, &region),
        credentials.clone(),
    )
    .expect("the proxy is an endpoint");

    // Three objects, so a listing bounded at one key has a page after the
    // page after the first.
    let key = test_key();
    let now = OffsetDateTime::now_utc();
    let mut stored_keys = Vec::new();
    for counter in 1..=3u64 {
        let (wire_bytes, signature) = seal(&key, &envelope_at(&key, counter, now));
        let submission = StoredSubmission {
            name: object_name(&key, now, Kind::Metrics),
            vkey: key.verifying_key(),
            signature,
            wire_bytes: &wire_bytes,
        };
        archive
            .store(&submission)
            .expect("the bucket accepts a PUT");
        stored_keys.push(submission.object_key());
        match counter {
            1 => recorder.record("put-accepted", &provenance),
            _ => recorder.forget(),
        }
    }
    stored_keys.sort();

    // The whole listing in one answer: what the audit walks.
    archive.keys().expect("the bucket lists");
    recorder.record("list-all", &provenance);

    // The same corpus a key to a page, each page asked for by the token the
    // last one gave.
    let bucket_at_proxy = recorder.bucket(&bucket, &region);
    let mut token = None;
    for page_number in 1..=3 {
        token = list_page(&bucket_at_proxy, &credentials, &agent, token);
        recorder.record(&format!("list-page-{page_number}"), &provenance);
    }
    assert!(token.is_none(), "the last page is still truncated");

    // The developer route's question: one bounded page from a cursor, which
    // S3 reads exclusively.
    archive
        .page(KEY_PREFIX, &stored_keys[0], nonzero_u32(50))
        .expect("the bucket pages");
    recorder.record("list-after", &provenance);

    // A listing that names nothing: what an empty bucket answers, which the
    // server has to tell from a bucket it could not read.
    archive
        .page("v1/nothing/", "", nonzero_u32(1000))
        .expect("the bucket pages");
    recorder.record("list-empty", &provenance);

    // A GET of a stored object: the body verbatim and the metadata headers
    // beside it.
    archive
        .fetch(&stored_keys[0])
        .expect("the object reads back");
    recorder.record("get-object", &provenance);

    // An object written by something that is not this server: no metadata
    // beside it, which `fetch` has to say rather than assume.
    let unadorned = object_name(&key, now, Kind::Metrics).to_key();
    let put = bucket_at_proxy
        .put_object(Some(&credentials), &unadorned)
        .sign(VALIDITY);
    agent
        .put(put.as_str())
        .send(b"an object nothing signed")
        .expect("the bucket accepts a PUT");
    recorder.forget();
    archive
        .fetch(&unadorned)
        .expect_err("the object carries no metadata");
    recorder.record("get-unadorned", &provenance);

    // A key the bucket does not hold, named the way a real one is.
    let missing = object_name(&key, now, Kind::Metrics).to_key();
    assert!(
        archive.reader(&missing).is_err(),
        "the bucket holds no such key"
    );
    recorder.record("get-missing", &provenance);

    // A bucket that does not exist, listed with credentials that are good for
    // another one.
    let elsewhere = S3Archive::new(
        &config_for(&recorder.endpoint, "no-such-bucket", &region),
        credentials,
    )
    .expect("the proxy is an endpoint");
    elsewhere.keys().expect_err("there is no such bucket");
    recorder.record("list-missing-bucket", &provenance);

    // A presigned PUT from a key the bucket grants nothing.
    let outsider = S3Archive::new(
        &config_for(&recorder.endpoint, &bucket, &region),
        Credentials::new(
            required("OUTSIDER_ACCESS_KEY_ID"),
            required("OUTSIDER_SECRET_ACCESS_KEY"),
        ),
    )
    .expect("the proxy is an endpoint");
    let (wire_bytes, signature) = seal(&key, &envelope_at(&key, 4, now));
    outsider
        .store(&StoredSubmission {
            name: object_name(&key, now, Kind::Metrics),
            vkey: key.verifying_key(),
            signature,
            wire_bytes: &wire_bytes,
        })
        .expect_err("the outsider is refused");
    recorder.record("put-refused", &provenance);
}
