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

// The exception the 4xx rule needs: a rate limit says come back, not fix
// something. Reading it as terminal is what sends an agent into the rejection
// backoff over a window that clears on its own.
#[test]
fn a_429_is_retryable() {
    let refusal = http::classify(&mut answered(429, "over the limit")).unwrap_err();
    assert_eq!(refusal.status, 429);
    assert_eq!(refusal.reason, "over the limit");
    assert!(refusal.retryable);
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

// What a proxy in front of the server answers. metsuke-server states a reason
// in a sentence, but an agent meets whatever is between it and the server
// first: this is the page nginx returned to leios1-bp-a-1's agent when its
// drain outran a rate limit. Logged as it arrived, journald reads each newline
// as its own entry, so the severity prefix written once at the front is
// attached to the first line and nothing else.
#[test]
fn a_proxys_html_page_is_reduced_to_one_line() {
    let page = "<html>\r\n<head><title>429 Too Many Requests</title></head>\r\n<body>\r\n\
                <center><h1>429 Too Many Requests</h1></center>\r\n<hr><center>nginx</center>\r\n\
                </body>\r\n</html>\r\n";
    let refusal = http::classify(&mut answered(429, page)).unwrap_err();
    assert!(
        !refusal.reason.contains('\n') && !refusal.reason.contains('\r'),
        "a reason has to be one journal entry, got {:?}",
        refusal.reason
    );
    assert!(
        refusal.reason.contains("429 Too Many Requests"),
        "and still say what answered, got {:?}",
        refusal.reason
    );
    assert!(refusal.retryable);
}

// A refusal is logged, so its length is a log line's length and not the
// answering server's to choose.
#[test]
fn a_reason_past_the_bound_is_cut_with_a_mark() {
    let refusal = http::classify(&mut answered(500, &"x".repeat(10_000))).unwrap_err();
    assert!(
        refusal.reason.chars().count() <= 201,
        "got {} chars",
        refusal.reason.chars().count()
    );
    assert!(
        refusal.reason.ends_with('…'),
        "a cut has to show, got {:?}",
        refusal.reason
    );
}

// Cutting inside a multi-byte character would panic on the slice, and a reason
// is whatever answered rather than something this crate chose the encoding of.
#[test]
fn a_reason_is_cut_on_a_character_boundary() {
    let refusal = http::classify(&mut answered(500, &"é".repeat(10_000))).unwrap_err();
    assert!(refusal.reason.starts_with('é'));
    assert!(refusal.reason.ends_with('…'));
}

// The common case is unchanged: one sentence arrives as itself.
#[test]
fn a_reason_that_is_already_a_line_is_left_alone() {
    let refusal = http::classify(&mut answered(403, "pool is not on the allowlist")).unwrap_err();
    assert_eq!(refusal.reason, "pool is not on the allowlist");
}
