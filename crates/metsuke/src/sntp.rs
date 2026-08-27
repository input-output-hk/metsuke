//! Per-scrape SNTP clock-offset probe (RFC 4330). One code path: a single
//! UDP exchange per configured server, first success wins; any failure
//! yields a null offset, never an aborted scrape.

use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;

use time::OffsetDateTime;

/// Shipped default SNTP server.
pub const DEFAULT_SERVER: &str = "time.cloudflare.com:123";

/// Seconds between the NTP era (1900-01-01) and the Unix epoch (1970-01-01),
/// RFC 4330 §3.
const NTP_UNIX_DELTA_SECS: i128 = 2_208_988_800;

pub struct SntpConfig {
    /// Servers as `host:port`, tried in order until one answers.
    pub servers: Vec<String>,
    /// Per-server deadline for the whole exchange.
    pub timeout: Duration,
}

/// Local-clock offset in milliseconds (positive = server ahead), or `None`
/// when no configured server produced a valid reply.
pub fn probe(config: &SntpConfig) -> Option<i64> {
    config
        .servers
        .iter()
        .find_map(|server| exchange(server, config.timeout))
}

/// One request/reply against one server; the RFC 4330 §5 offset
/// `((T2 - T1) + (T3 - T4)) / 2` from its reply.
fn exchange(server: &str, timeout: Duration) -> Option<i64> {
    let address = server.to_socket_addrs().ok()?.next()?;
    let socket = UdpSocket::bind(unspecified(&address)).ok()?;
    socket.set_read_timeout(Some(timeout)).ok()?;
    socket.connect(address).ok()?;

    let t1 = OffsetDateTime::now_utc();
    let mut request = [0u8; 48];
    request[0] = 0x23; // LI 0, version 4, mode 3 (client)
    request[40..48].copy_from_slice(&to_ntp(t1).to_be_bytes());
    socket.send(&request).ok()?;

    let mut reply = [0u8; 48];
    let read = socket.recv(&mut reply).ok()?;
    let t4 = OffsetDateTime::now_utc();
    if read < 48 {
        return None;
    }
    // RFC 4330 §5 check 1: the originate timestamp must echo our transmit
    // timestamp, or the reply is not an answer to this request.
    if reply[24..32] != request[40..48] {
        return None;
    }
    // RFC 4330 §5: a unicast reply carries mode 4 (server).
    if reply[0] & 0b0000_0111 != 4 {
        return None;
    }
    // RFC 4330 §8: stratum 0 is a kiss-o'-death packet, not a time answer.
    if reply[1] == 0 {
        return None;
    }
    // RFC 4330 §5: leap indicator 3 means the server clock is unsynchronized.
    if reply[0] >> 6 == 3 {
        return None;
    }

    let t2 = u64::from_be_bytes(reply[32..40].try_into().expect("8-byte slice"));
    let t3 = u64::from_be_bytes(reply[40..48].try_into().expect("8-byte slice"));
    let offset_ns = ((from_ntp(t2) - t1.unix_timestamp_nanos())
        + (from_ntp(t3) - t4.unix_timestamp_nanos()))
        / 2;
    i64::try_from(offset_ns / 1_000_000).ok()
}

/// The wildcard bind address in `peer`'s family, so the socket can reach it.
fn unspecified(peer: &SocketAddr) -> SocketAddr {
    match peer {
        SocketAddr::V4(_) => SocketAddr::from(([0, 0, 0, 0], 0)),
        SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
    }
}

/// NTP 32.32 fixed-point timestamp for a wall-clock instant.
fn to_ntp(t: OffsetDateTime) -> u64 {
    let nanos = t.unix_timestamp_nanos() + NTP_UNIX_DELTA_SECS * 1_000_000_000;
    let secs = (nanos.div_euclid(1_000_000_000)) as u64;
    let frac = (nanos.rem_euclid(1_000_000_000) as u64) * (1 << 32) / 1_000_000_000;
    (secs << 32) | frac
}

/// Unix nanoseconds for an NTP 32.32 fixed-point timestamp.
fn from_ntp(ntp: u64) -> i128 {
    let secs = (ntp >> 32) as i128 - NTP_UNIX_DELTA_SECS;
    let frac_ns = ((ntp & 0xffff_ffff) * 1_000_000_000) >> 32;
    secs * 1_000_000_000 + frac_ns as i128
}
