use metsuke_wire::http;

fn answered(status: u16, body: &str) -> ureq::http::Response<ureq::Body> {
    ureq::http::Response::builder()
        .status(status)
        .body(ureq::Body::builder().data(body))
        .unwrap()
}

#[test]
fn a_2xx_leaves_the_body_for_the_caller() {
    let mut response = answered(200, "the ack");
    http::classify(&mut response).unwrap();
    assert_eq!(response.body_mut().read_to_string().unwrap(), "the ack");
}

#[test]
fn a_2xx_without_a_body_is_still_success() {
    assert!(http::classify(&mut answered(204, "")).is_ok());
}

#[test]
fn a_4xx_is_terminal_and_carries_the_body_as_the_reason() {
    let refusal = http::classify(&mut answered(403, "pool not on the allowlist")).unwrap_err();
    assert_eq!(refusal.status, 403);
    assert_eq!(refusal.reason, "pool not on the allowlist");
    assert!(!refusal.retryable);
}

#[test]
fn a_5xx_is_retryable() {
    let refusal = http::classify(&mut answered(503, "come back later")).unwrap_err();
    assert_eq!(refusal.status, 503);
    assert_eq!(refusal.reason, "come back later");
    assert!(refusal.retryable);
}

// Only a 4xx is terminal: a redirect the agent declined to follow is the
// same scheduling decision as a 5xx, which is what the agent's backoff
// (metsuke::schedule) was written against.
#[test]
fn a_3xx_is_retryable() {
    assert!(
        http::classify(&mut answered(302, ""))
            .unwrap_err()
            .retryable
    );
}
