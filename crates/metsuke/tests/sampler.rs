//! Seam test for sample assembly (ticket metsuke-4zo.3): one `sample()`
//! call against a recorded metrics body and a scripted SNTP server yields
//! a `Sample` carrying both the scraped metrics and the clock offset.

use std::net::UdpSocket;
use std::time::Duration;

use metsuke::sampler::{SamplerConfig, sample};
use metsuke::scrape::ScrapeConfig;
use metsuke::sntp::SntpConfig;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RECORDED_CHAIN: &str = include_str!("fixtures/recordings/leios-node.prom");

/// Serve one well-formed SNTP reply claiming the server clock is 5 s ahead:
/// receive and transmit timestamps are the client's echoed transmit
/// timestamp shifted by 5 s in 32.32 fixed point.
fn five_seconds_ahead_server() -> String {
    let socket = UdpSocket::bind("127.0.0.1:0").expect("bind loopback");
    let addr = socket.local_addr().expect("local addr").to_string();
    std::thread::spawn(move || {
        let mut request = [0u8; 48];
        let (_, peer) = socket.recv_from(&mut request).expect("recv request");
        let client_transmit = u64::from_be_bytes(request[40..48].try_into().unwrap());
        let shifted = client_transmit.wrapping_add(5 << 32);
        let mut reply = [0u8; 48];
        reply[0] = 0x24; // LI 0, version 4, mode 4 (server)
        reply[1] = 2; // stratum
        reply[24..32].copy_from_slice(&request[40..48]); // originate = echoed transmit
        reply[32..40].copy_from_slice(&shifted.to_be_bytes()); // receive
        reply[40..48].copy_from_slice(&shifted.to_be_bytes()); // transmit
        socket.send_to(&reply, peer).expect("send reply");
    });
    addr
}

#[tokio::test]
async fn one_sample_carries_metrics_and_clock_offset() {
    let metrics = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metrics"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(RECORDED_CHAIN, "text/plain;version=0.0.4;charset=utf-8"),
        )
        .mount(&metrics)
        .await;
    let config = SamplerConfig {
        scrape: ScrapeConfig {
            metrics_url: format!("{}/metrics", metrics.uri()),
            timeout: Duration::from_secs(5),
            max_body_bytes: 1024 * 1024,
        },
        sntp: SntpConfig {
            servers: vec![five_seconds_ahead_server()],
            timeout: Duration::from_secs(2),
        },
    };
    let sample = tokio::task::spawn_blocking(move || sample(&config))
        .await
        .expect("sampler task panicked");
    // Recorded-body field values: tests/scrape.rs.
    assert_eq!(sample.block_height, Some(5));
    let offset = sample.clock_offset_ms.expect("probe should succeed");
    assert!(
        (4900..=5000).contains(&offset),
        "offset {offset} ms outside the expected 5 s band"
    );
}
