//! Seal one submission of the named payload shape and print its wire bytes as
//! lowercase hex. Driven by scripts/record-submission.sh, which is what writes
//! the recordings under tests/fixtures; the values live here so the recording
//! stays the only copy of the bytes.

use metsuke_wire::envelope::{
    AgentId, Envelope, Payload, PoolId, Sample, SigningKey, TraceLine, seal,
};
use time::OffsetDateTime;

fn main() {
    let shape = std::env::args()
        .nth(1)
        .expect("usage: record-submission <samples|lines>");
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let at =
        OffsetDateTime::from_unix_timestamp(1_755_000_000).expect("a fixed instant is in range");
    // Every optional field is set, so a build that drops or renames one cannot
    // reseal the recording unchanged.
    let payload = match shape.as_str() {
        "samples" => Payload::Samples {
            samples: vec![Sample {
                sampled_at: at,
                block_height: Some(12_318_442),
                slot: Some(163_281_005),
                slot_in_epoch: Some(281_005),
                epoch: Some(587),
                sync_progress: Some(0.5),
                node_version: Some("11.0.1".to_string()),
                node_revision: Some("0e2b4b1a".to_string()),
                clock_offset_ms: Some(-3),
            }],
        },
        // Two lines, so the framing between them is in the recording and not
        // only the one that terminates the payload.
        "lines" => Payload::Lines {
            lines: [
                r#"{"at":"2026-08-12T07:20:00.00Z","ns":"Leios.Announcement","sev":"Info","data":{"slot":163281005}}"#,
                r#"{"at":"2026-08-12T07:20:01.00Z","ns":"Leios.NotVoted","sev":"Notice","data":{"reason":"NoQuorum"}}"#,
            ]
            .map(|line| TraceLine::parse(line).expect("a recorded trace line is a JSON object"))
            .to_vec(),
        },
        other => panic!("unknown payload shape {other:?}"),
    };
    let envelope = Envelope::new(
        PoolId::from_cold_key(&key.verifying_key()),
        AgentId::slugify("relay-1").expect("a fixed name slugifies"),
        "0.1.0".to_string(),
        42,
        at,
        payload,
    );
    let (bytes, _) = seal(&key, &envelope, 0).expect("the recorded envelope seals");
    println!("{}", metsuke_wire::hex::encode(&bytes));
}
