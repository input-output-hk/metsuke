//! Seal one submission of the named payload shape and print its wire bytes as
//! lowercase hex. Driven by scripts/record-submission.sh, which is what writes
//! the recordings under tests/fixtures; the values live here so the recording
//! stays the only copy of the bytes.

use std::collections::BTreeMap;

use serde_json::Number;

use metsuke_wire::envelope::{
    AgentId, Envelope, Failure, Metric, Payload, PayloadLine, PoolId, Provenance, Reason, Scrape,
    SigningKey, TraceLine, seal,
};
use time::OffsetDateTime;

fn main() {
    let shape = std::env::args()
        .nth(1)
        .expect("usage: record-submission <scrapes|lines>");
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let at =
        OffsetDateTime::from_unix_timestamp(1_755_000_000).expect("a fixed instant is in range");
    let provenance = Provenance {
        pool_id: PoolId::from_cold_key(&key.verifying_key()),
        agent_id: AgentId::slugify("relay-1").expect("a fixed name slugifies"),
    };
    // Every optional field is set, so a build that drops or renames one cannot
    // reseal the recording unchanged. Two scrape lines, because the two a
    // scrape can be — metrics, and the reason there are none — render
    // differently.
    let payload = match shape.as_str() {
        "scrapes" => Payload::scrapes(
            [
                Scrape {
                    scraped_at: at,
                    clock_offset_ms: Some(-3),
                    failure: None,
                    metrics: vec![
                        Metric {
                            name: "cardano_node_metrics_blockNum_int".to_string(),
                            labels: BTreeMap::new(),
                            value: 12_318_442.into(),
                            declared_type: Some("gauge".to_string()),
                        },
                        // Labels, a float, and no declared type: the three
                        // things the first metric does not pin.
                        Metric {
                            name: "cardano_node_metrics_density_real".to_string(),
                            labels: BTreeMap::from([("era".to_string(), "Conway".to_string())]),
                            value: Number::from_f64(0.5).expect("a finite float is a number"),
                            declared_type: None,
                        },
                    ],
                },
                Scrape {
                    scraped_at: at,
                    clock_offset_ms: None,
                    failure: Some(Failure {
                        reason: Reason::Refused,
                        detail: "the endpoint answered 503 (upstream is down)".to_string(),
                    }),
                    metrics: Vec::new(),
                },
            ]
            .map(|scrape| {
                PayloadLine::scrape(&scrape, &provenance).expect("the recorded scrape stamps")
            })
            .to_vec(),
        ),
        // Two lines, so the framing between them is in the recording and not
        // only the one that terminates the payload.
        "lines" => Payload::trace_lines(
            [
                r#"{"at":"2026-08-12T07:20:00.00Z","ns":"Leios.Announcement","sev":"Info","data":{"slot":163281005}}"#,
                r#"{"at":"2026-08-12T07:20:01.00Z","ns":"Leios.NotVoted","sev":"Notice","data":{"reason":"NoQuorum"}}"#,
            ]
            .map(|line| {
                let line = TraceLine::parse(line).expect("a recorded trace line is a JSON object");
                PayloadLine::trace_line(&line, &provenance).expect("a parsed line stamps")
            })
            .to_vec(),
        ),
        other => panic!("unknown payload shape {other:?}"),
    };
    let envelope = Envelope::new(provenance, "0.1.0".to_string(), 42, at, payload);
    let (bytes, _) = seal(&key, &envelope, 0).expect("the recorded envelope seals");
    println!("{}", metsuke_wire::hex::encode(&bytes));
}
