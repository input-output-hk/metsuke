//! SNTP probe tests against a scripted local UDP server (ticket
//! metsuke-4zo.3): offset math per the RFC 4330 formula, and every reply
//! defect degrading to a null offset.

use std::net::UdpSocket;
use std::time::Duration;

use metsuke::sntp::{SntpConfig, probe};

/// Serve one SNTP reply built by `reply` from the 48-byte request, on a
/// loopback port. Returns the server address as a config entry.
fn one_shot_server(reply: impl FnOnce(&[u8; 48]) -> Vec<u8> + Send + 'static) -> String {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind loopback");
    let addr = socket.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let mut request = [0u8; 48];
        let (read, peer) = socket.recv_from(&mut request).expect("recv request");
        assert_eq!(read, 48, "client request must be exactly 48 bytes");
        socket.send_to(&reply(&request), peer).expect("send reply");
    });
    addr.to_string()
}

/// A well-formed server reply claiming its clock is `offset_secs` ahead of
/// the client: receive and transmit timestamps are the client's transmit
/// timestamp shifted by `offset_secs` in 32.32 fixed point.
fn reply_with_offset(request: &[u8; 48], offset_secs: i64) -> Vec<u8> {
    let client_transmit = u64::from_be_bytes(request[40..48].try_into().unwrap());
    let shifted = client_transmit.wrapping_add_signed(offset_secs << 32);
    let mut reply = [0u8; 48];
    reply[0] = 0x24; // LI 0, version 4, mode 4 (server)
    reply[1] = 2; // stratum
    reply[24..32].copy_from_slice(&request[40..48]); // originate = echoed transmit
    reply[32..40].copy_from_slice(&shifted.to_be_bytes()); // receive
    reply[40..48].copy_from_slice(&shifted.to_be_bytes()); // transmit
    reply.to_vec()
}

fn config(servers: Vec<String>) -> SntpConfig {
    SntpConfig {
        servers,
        timeout: Duration::from_secs(2),
    }
}

/// A reply whose originate timestamp is not the transmit timestamp we sent
/// could belong to anyone (RFC 4330 §5 check 1); it must not produce an
/// offset.
#[test]
fn reply_not_echoing_our_transmit_yields_none() {
    let server = one_shot_server(|request| {
        let mut reply = reply_with_offset(request, 5);
        reply[31] ^= 0x01; // corrupt the echoed originate timestamp
        reply
    });
    assert_eq!(probe(&config(vec![server])), None);
}

/// A unicast reply must be mode 4 (server); anything else is not an SNTP
/// answer.
#[test]
fn reply_with_non_server_mode_yields_none() {
    let server = one_shot_server(|request| {
        let mut reply = reply_with_offset(request, 5);
        reply[0] = 0x23; // mode 3 (client) instead of 4
        reply
    });
    assert_eq!(probe(&config(vec![server])), None);
}

/// Stratum 0 is a kiss-o'-death packet (RFC 4330 §8): the server is telling
/// us to go away, not what time it is.
#[test]
fn kiss_of_death_reply_yields_none() {
    let server = one_shot_server(|request| {
        let mut reply = reply_with_offset(request, 5);
        reply[1] = 0; // stratum 0
        reply
    });
    assert_eq!(probe(&config(vec![server])), None);
}

/// Leap indicator 3 means the server's own clock is unsynchronized
/// (RFC 4330 §5); its timestamps are not usable.
#[test]
fn unsynchronized_server_reply_yields_none() {
    let server = one_shot_server(|request| {
        let mut reply = reply_with_offset(request, 5);
        reply[0] |= 0b1100_0000; // LI 3
        reply
    });
    assert_eq!(probe(&config(vec![server])), None);
}

/// A reply shorter than the 48-byte SNTP header cannot carry the
/// timestamps.
#[test]
fn truncated_reply_yields_none() {
    let server = one_shot_server(|request| reply_with_offset(request, 5)[..40].to_vec());
    assert_eq!(probe(&config(vec![server])), None);
}

/// A server that never answers is a failed probe, not an aborted sample.
#[test]
fn silent_server_yields_none() {
    let silent = UdpSocket::bind("127.0.0.1:0").expect("bind loopback");
    let address = silent.local_addr().expect("local addr").to_string();
    let mut short = config(vec![address]);
    short.timeout = Duration::from_millis(100);
    assert_eq!(probe(&short), None);
}

/// The servers are a fallback list: when the first never answers, the
/// second one's reply still produces an offset.
#[test]
fn second_server_answers_when_first_is_silent() {
    let silent = UdpSocket::bind("127.0.0.1:0").expect("bind loopback");
    let answering = one_shot_server(|request| reply_with_offset(request, 5));
    let mut cfg = config(vec![
        silent.local_addr().expect("local addr").to_string(),
        answering,
    ]);
    cfg.timeout = Duration::from_millis(100);
    let offset = probe(&cfg).expect("second server should answer");
    assert!((4900..=5000).contains(&offset));
}

/// A server clock 5 s behind yields a negative offset: the sign convention
/// is server minus local.
#[test]
fn server_five_seconds_behind_yields_negative_offset() {
    let server = one_shot_server(|request| reply_with_offset(request, -5));
    let offset = probe(&config(vec![server])).expect("probe should succeed");
    assert!(
        (-5100..=-5000).contains(&offset),
        "offset {offset} ms outside the expected -5 s band"
    );
}

/// The reply says the server clock is 5 s ahead; the measured offset is
/// 5000 ms minus half the loopback round trip, so a tight band below
/// 5000 ms catches both a broken sign and broken fixed-point scaling.
#[test]
fn server_five_seconds_ahead_yields_positive_offset() {
    let server = one_shot_server(|request| reply_with_offset(request, 5));
    let offset = probe(&config(vec![server])).expect("probe should succeed");
    assert!(
        (4900..=5000).contains(&offset),
        "offset {offset} ms outside the expected 5 s band"
    );
}
