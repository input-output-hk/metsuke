//! The transport: hyper over tokio, and the only module that names an HTTP
//! type. It decodes a request into `http::Request`, hands that to
//! `http::answer`, and writes the `http::Answer` back.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt as _, Full};
use hyper::body::{Body, Frame, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use metsuke_wire::envelope::{HEADER_POOL, HEADER_SIGNATURE, HEADER_VKEY, PoolId};
use metsuke_wire::journal::{ERR, WARNING};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc};

use crate::archive::{Bytes as ArchiveBytes, List, ObjectStream, Store};
use crate::authority::Attributed;
use crate::config::HttpConfig;
use crate::developer::Developer;
use crate::http::{self, Answer, AnswerBody, Method, Request};
use crate::instructions;
use crate::intake::Intake;

/// How much of an object is moved per read: copy granularity, not a bound
/// anything can observe (CLAUDE.md `## Conventions`). Never a frame size. No
/// download is chunked (`archive::ObjectStream`).
const DOWNLOAD_CHUNK_BYTES: usize = 64 * 1024;

/// Everything one request is answered from, shared by every connection.
struct Serving<A: Store> {
    intake: Intake<A>,
    developer: Developer,
    pages: http::Pages,
}

/// A bound listener that has not been served yet. Two steps because the
/// startup line names the address the kernel chose (`main::serve`).
pub struct Listener {
    runtime: tokio::runtime::Runtime,
    listener: TcpListener,
    address: SocketAddr,
}

/// Bind `listen`, on a runtime this owns. One thread for the accept loop and
/// the connections, plus the blocking pool everything that touches the archive
/// runs on: the archive is reached with ureq, so a store or a fetch would
/// otherwise stop every other connection this thread is serving. Nothing left
/// on the async side blocks, so a second scheduler thread would buy nothing.
pub fn bind(listen: &str) -> Result<Listener, io::Error> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    let listener = runtime.block_on(TcpListener::bind(listen))?;
    let address = listener.local_addr()?;
    Ok(Listener {
        runtime,
        listener,
        address,
    })
}

impl Listener {
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Serve until accepting fails in a way this process cannot serve through
    /// (`one_connection`). Only returns on that.
    pub fn serve<A: Store + ArchiveBytes + List + Send + Sync + 'static>(
        self,
        limits: HttpConfig,
        intake: Intake<A>,
        developer: Developer,
        pages: instructions::Pages,
    ) -> Result<std::convert::Infallible, io::Error> {
        let serving = Arc::new(Serving {
            intake,
            developer,
            pages: http::Pages::from(pages),
        });
        self.runtime
            .block_on(accept(self.listener, limits, serving))
    }
}

async fn accept<A: Store + ArchiveBytes + List + Send + Sync + 'static>(
    listener: TcpListener,
    limits: HttpConfig,
    serving: Arc<Serving<A>>,
) -> Result<std::convert::Infallible, io::Error> {
    let idle = millis(limits.idle_timeout_ms.get());
    let write_timeout = millis(limits.write_timeout_ms.get());
    let read_timeout = millis(limits.read_timeout_ms.get());
    let slots = Arc::new(Semaphore::new(limits.max_concurrent_requests.get() as usize));
    loop {
        // The permit is taken before the accept, not after: past the cap the
        // next connection waits in the kernel's backlog, where it costs this
        // process nothing.
        let slot = Arc::clone(&slots)
            .acquire_owned()
            .await
            .expect("the semaphore is never closed while the loop runs");
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            // Losing every submission in flight over one client would be the
            // wrong trade (`one_connection`).
            Err(error) if one_connection(&error) => {
                eprintln!("{WARNING}a connection was not accepted: {error}");
                continue;
            }
            // Everything else, descriptor exhaustion above all, is the
            // host's, and a process that keeps looping on it accepts nothing
            // while looking healthy. Exiting hands the wait to systemd, which
            // holds a restart for `RestartSec` (nix/unit.nix).
            Err(error) => return Err(error),
        };
        let serving = Arc::clone(&serving);
        tokio::task::spawn(async move {
            let _slot = slot;
            // Whose the last request answered on this connection was. A
            // failure lands at the connection rather than at an answer, so
            // this is what lets the log name a pool at all; the peer address
            // cannot, being the proxy's for every pool.
            let last_answered = Arc::new(std::sync::Mutex::new(None));
            let service = service_fn({
                let last_answered = Arc::clone(&last_answered);
                move |request| {
                    handle(
                        Arc::clone(&serving),
                        read_timeout,
                        Arc::clone(&last_answered),
                        request,
                    )
                }
            });
            let served = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(idle)
                .serve_connection(
                    TokioIo::new(WriteDeadline::new(stream, write_timeout)),
                    service,
                )
                .await;
            // An idle connection reaching `idle_timeout_ms` is the configured
            // behaviour and no news. Everything else is: a connection that
            // ended early may have lost an ack, and an agent that never saw
            // one sends the submission again.
            if let Err(error) = served
                && !error.is_timeout()
            {
                let signer = *last_answered.lock().expect("no panic holds this lock");
                eprintln!(
                    "{WARNING}the connection last answering {} ended: {error}",
                    http::named(signer)
                );
            }
        });
    }
}

/// Whether an accept failure was about the connection that failed rather than
/// about this process's ability to take any. `ECONNABORTED` and `EINTR`, plus
/// the pending network errors accept(2) says a reliable application treats as
/// `EAGAIN`. Each is dequeued with the connection that carried it, so none can
/// be handed back twice and none can spin this loop.
///
/// `EPERM` is not here. An LSM refusing accept(2) leaves the connection
/// queued, so continuing would spin; and it is host policy rather than one
/// client, which is the exit path's case.
///
/// Three of accept(2)'s errnos have no `ErrorKind` and reach here as
/// `Uncategorized`: `EPROTO`, `ENOPROTOOPT` and `EHOSTDOWN`. They are
/// therefore not covered and exit; matching them takes raw errnos, which this
/// would then have to keep true per platform for a case a restart already
/// recovers from.
fn one_connection(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::Interrupted
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    )
}

fn millis(count: u64) -> Duration {
    Duration::from_millis(count)
}

/// Decode, answer, encode.
async fn handle<A: Store + ArchiveBytes + List + Send + Sync + 'static>(
    serving: Arc<Serving<A>>,
    read_timeout: Duration,
    last_answered: Arc<std::sync::Mutex<Option<PoolId>>>,
    request: hyper::Request<Incoming>,
) -> Result<hyper::Response<ResponseBody>, std::convert::Infallible> {
    let (parts, body) = request.into_parts();
    let method = match parts.method {
        hyper::Method::GET => Method::Get,
        hyper::Method::POST => Method::Post,
        _ => Method::Other,
    };
    let target = parts
        .uri
        .path_and_query()
        .map(|target| target.as_str())
        .unwrap_or("/")
        .to_string();
    // A header value that is not text cannot be a hex key or a Basic
    // credential, so it is absent rather than a second way to be malformed.
    let text = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let decoded = Request {
        method,
        submission: Attributed::decode(
            text(HEADER_VKEY).as_deref(),
            text(HEADER_SIGNATURE).as_deref(),
            text(HEADER_POOL).as_deref(),
        ),
        authorization: text("authorization"),
        body: Vec::new(),
        target,
    };
    let signer = takes_a_body(&decoded);
    *last_answered.lock().expect("no panic holds this lock") = signer;
    // A body is read for one route, and only once its headers have decoded:
    // what a client declares is never allocated, and never waited for on a
    // request already answerable (metsuke-a3a). Every other answer drops the
    // `Incoming` unread.
    let answer = match signer {
        None => blocking(serving, decoded).await,
        Some(_) => {
            let max = serving.intake.max_body_bytes();
            match read_body(body, &parts.headers, max, read_timeout, signer).await {
                Ok(body) => blocking(serving, Request { body, ..decoded }).await,
                Err(refused) => refused,
            }
        }
    };
    Ok(respond(answer))
}

/// Whose body to read, where there is one to read.
fn takes_a_body(request: &Request) -> Option<PoolId> {
    match request.method == Method::Post && request.path() == http::SUBMIT_PATH {
        true => request.submission.as_ref().ok().map(|it| it.pool_id()),
        false => None,
    }
}

async fn blocking<A: Store + ArchiveBytes + List + Send + Sync + 'static>(
    serving: Arc<Serving<A>>,
    request: Request,
) -> Answer {
    tokio::task::spawn_blocking(move || {
        http::answer(&serving.intake, &serving.developer, &serving.pages, request)
    })
    .await
    .unwrap_or_else(|error| {
        // A panic in `http::answer`, which nothing there is meant to do. The
        // client gets a 503 rather than a dropped connection it would read as
        // a network fault.
        eprintln!("{ERR}answering panicked: {error}");
        http::refuse(None, 503, "the server could not answer".to_string())
    })
}

/// Read the body, bounded by `max_body_bytes` and by the clock.
///
/// `Content-Length` catches the honest oversized upload before a byte of it is
/// read; `bounded` catches everything else.
async fn read_body(
    body: Incoming,
    headers: &hyper::HeaderMap,
    max: u64,
    read_timeout: Duration,
    signer: Option<PoolId>,
) -> Result<Vec<u8>, Answer> {
    if let Some(declared) = declared_length(headers)
        && declared > max
    {
        return Err(http::oversized(signer, declared as usize, max));
    }
    match tokio::time::timeout(read_timeout, bounded(body, max, signer)).await {
        Ok(Ok(body)) => Ok(body),
        Ok(Err(refused)) => Err(refused),
        Err(_) => Err(http::refuse(
            signer,
            408,
            format!(
                "the request body did not arrive within {}s",
                read_timeout.as_secs()
            ),
        )),
    }
}

fn declared_length(headers: &hyper::HeaderMap) -> Option<u64> {
    headers
        .get(hyper::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// Frames appended until the body ends or passes `max`. Stopping at the frame
/// that crosses the cap is what bounds a chunked body, which declares no
/// length to refuse in advance.
async fn bounded(mut body: Incoming, max: u64, signer: Option<PoolId>) -> Result<Vec<u8>, Answer> {
    let mut collected = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            http::refuse(
                signer,
                400,
                format!("could not read the request body: {error}"),
            )
        })?;
        if let Some(data) = frame.data_ref() {
            collected.extend_from_slice(data);
            if collected.len() as u64 > max {
                return Err(http::oversized(signer, collected.len(), max));
            }
        }
    }
    Ok(collected)
}

type ResponseBody = BoxBody<Bytes, io::Error>;

fn respond(answer: Answer) -> hyper::Response<ResponseBody> {
    let Answer {
        status,
        content_type,
        body,
        headers,
    } = answer;
    let mut response = hyper::Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type);
    for (field, value) in &headers {
        response = response.header(*field, value);
    }
    let body = match body {
        // No Content-Length set here: a body already in hand reports its own
        // size and hyper writes the header from that.
        AnswerBody::Bytes(bytes) => Full::new(bytes).map_err(|never| match never {}).boxed(),
        // A stream does not, so what the archive declared is stated instead.
        AnswerBody::Stream(stream) => {
            response = response.header(hyper::header::CONTENT_LENGTH, stream.length);
            streamed(*stream).boxed()
        }
    };
    response
        .body(body)
        .expect("a status and headers this server writes are well formed")
}

/// The archive's reader drained on the blocking pool into the frames hyper
/// writes. A one-frame channel is the backpressure: the reader only pulls the
/// next chunk once the last one has gone out, so a developer that stops
/// reading stalls its own download and nothing else.
fn streamed(stream: ObjectStream) -> Streamed {
    // The attestation went out with the head (`http::attested`), so what is
    // left here is the body and what bounds it.
    let ObjectStream {
        key,
        length,
        mut reader,
        ..
    } = stream;
    let (frames, receiver) = mpsc::channel(1);
    tokio::task::spawn_blocking(move || {
        let mut buffer = vec![0u8; DOWNLOAD_CHUNK_BYTES];
        let mut sent = 0u64;
        // The head is out, `length` with it, so nothing below can correct the
        // answer. Each arm's account of what went wrong is the log line, which
        // names the key because the connection-level one cannot.
        loop {
            let chunk = match reader.read(&mut buffer) {
                Ok(0) => {
                    if sent < length {
                        eprintln!(
                            "{ERR}the download of {key} ended {} bytes short of the {length} it declared",
                            length - sent
                        );
                    }
                    return;
                }
                Ok(read) => {
                    sent += read as u64;
                    // hyper truncates a body that runs past `Content-Length`
                    // rather than failing, so an object that grew since its
                    // length was read would go out as a whole-looking answer
                    // that is not the object. Cut it here, where it can be
                    // said.
                    if sent > length {
                        eprintln!(
                            "{ERR}the download of {key} grew past the {length} it declared; cut at {} bytes",
                            sent - read as u64
                        );
                        return;
                    }
                    Ok(Frame::data(Bytes::copy_from_slice(&buffer[..read])))
                }
                // Cutting the body short is the only report left. Under
                // Content-Length the client sees a truncated answer, which is
                // the honest one: what it holds is not the object.
                Err(error) => {
                    eprintln!("{ERR}the download of {key} stopped: {error}");
                    Err(error)
                }
            };
            let failed = chunk.is_err();
            if frames.blocking_send(chunk).is_err() || failed {
                return;
            }
        }
    });
    Streamed { frames: receiver }
}

/// The body half of `streamed`: whatever the blocking reader has produced.
struct Streamed {
    frames: mpsc::Receiver<Result<Frame<Bytes>, io::Error>>,
}

impl Body for Streamed {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        self.frames.poll_recv(context)
    }
}

/// A connection whose writes have to make progress, because hyper has no write
/// timeout of its own (`config::HttpConfig::write_timeout_ms`).
struct WriteDeadline {
    stream: TcpStream,
    limit: Duration,
    /// Armed on the first write that cannot proceed and disarmed by the next
    /// one that can, so the bound is on a stalled write rather than on a slow
    /// answer that is still moving.
    stalled: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl WriteDeadline {
    fn new(stream: TcpStream, limit: Duration) -> WriteDeadline {
        WriteDeadline {
            stream,
            limit,
            stalled: None,
        }
    }

    /// Whatever the inner stream answered, with the deadline armed while it is
    /// `Pending` and expiring into a timeout.
    fn progressing<T>(
        &mut self,
        context: &mut Context<'_>,
        polled: Poll<io::Result<T>>,
    ) -> Poll<io::Result<T>> {
        match polled {
            Poll::Ready(answer) => {
                self.stalled = None;
                Poll::Ready(answer)
            }
            Poll::Pending => {
                let limit = self.limit;
                let stalled = self
                    .stalled
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep(limit)));
                match stalled.as_mut().poll(context) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("the client read nothing for {}s", limit.as_secs()),
                    ))),
                }
            }
        }
    }
}

impl AsyncRead for WriteDeadline {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(context, buffer)
    }
}

impl AsyncWrite for WriteDeadline {
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.stream).poll_write(context, buffer);
        this.progressing(context, polled)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.stream).poll_flush(context);
        this.progressing(context, polled)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.stream).poll_shutdown(context);
        this.progressing(context, polled)
    }
}

/// `one_connection`'s split, which is unreachable over a socket: nothing a test
/// can do makes accept(2) return these.
#[cfg(test)]
mod tests {
    use super::one_connection;
    use std::io::Error;

    #[test]
    fn a_failure_carried_by_one_connection_is_skipped() {
        for errno in [ECONNABORTED, EINTR, ENETDOWN, ENETUNREACH, EHOSTUNREACH] {
            let error = Error::from_raw_os_error(errno);
            assert!(
                one_connection(&error),
                "{errno}: {error} ({:?})",
                error.kind()
            );
        }
    }

    #[test]
    fn a_failure_about_the_process_is_not_skipped() {
        for errno in [EPERM, EMFILE] {
            let error = Error::from_raw_os_error(errno);
            assert!(
                !one_connection(&error),
                "{errno}: {error} ({:?})",
                error.kind()
            );
        }
    }

    // The errnos above, as Linux numbers them. Written out rather than pulled
    // from a crate: `one_connection` matches on `ErrorKind`, and what these
    // check is that the errno an operator would see maps where it expects.
    const EPERM: i32 = 1;
    const EINTR: i32 = 4;
    const EMFILE: i32 = 24;
    const ENETDOWN: i32 = 100;
    const ENETUNREACH: i32 = 101;
    const ECONNABORTED: i32 = 103;
    const EHOSTUNREACH: i32 = 113;
}
