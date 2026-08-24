//! Developer pull access (`metsuke_server::developer`): who may read the
//! archive, what a filter may ask for, and the JSON one page answers as.

use base64::Engine as _;
use metsuke_server::archive::ObjectName;
use metsuke_server::developer::{Developer, Filters, page, query_value};
use metsuke_server::index::Listing;

mod support;
use support::{DEVELOPER_PASSWORD, developer_config, pool_of, test_key, test_now};

fn developer() -> Developer {
    let dir = tempfile::tempdir().unwrap();
    Developer::new(&developer_config(dir.path()), DEVELOPER_PASSWORD)
}

/// The `Authorization` header a client sends for these credentials.
fn basic(user: &str, password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {encoded}")
}

/// The user the shipped example names, which is what `developer_config` loads.
fn configured_user() -> String {
    developer_config(std::path::Path::new("/tmp")).user
}

#[test]
fn the_configured_credentials_are_authorized() {
    let header = basic(&configured_user(), DEVELOPER_PASSWORD);
    assert!(developer().authorize(Some(&header)).is_ok());
}

/// The gate has to be closed by default: a request that says nothing about who
/// it is must not read the archive.
#[test]
fn a_request_without_an_authorization_header_is_refused() {
    assert!(developer().authorize(None).is_err());
}

#[test]
fn a_wrong_password_is_refused() {
    let header = basic(&configured_user(), "not the password");
    assert!(developer().authorize(Some(&header)).is_err());
}

#[test]
fn another_user_with_the_right_password_is_refused() {
    let header = basic("someone-else", DEVELOPER_PASSWORD);
    assert!(developer().authorize(Some(&header)).is_err());
}

/// Everything that is not a well-formed Basic credential, refused the same way
/// as a wrong one: telling a client which of the two it got wrong tells it
/// whether the user exists.
#[test]
fn a_header_that_is_not_basic_credentials_is_refused() {
    for header in [
        "",
        "Basic",
        "Basic ",
        "Bearer token",
        // Base64 of a string with no colon in it.
        "Basic aHVudGVyMg==",
        // Not base64 at all.
        "Basic ****",
    ] {
        assert!(
            developer().authorize(Some(header)).is_err(),
            "{header:?} must not authorize"
        );
    }
}

/// The scheme is case-insensitive (RFC 7235), and a client that capitalises it
/// differently is not an unauthorized one.
#[test]
fn the_scheme_is_matched_case_insensitively() {
    let header = basic(&configured_user(), DEVELOPER_PASSWORD).replace("Basic", "basic");
    assert!(developer().authorize(Some(&header)).is_ok());
}

#[test]
fn a_prefix_and_an_after_are_read_off_the_query() {
    let filters =
        Filters::parse("/v1/submissions?prefix=v1/pool1abc/&after=v1/pool1abc/x").unwrap();
    assert_eq!(filters.prefix, "v1/pool1abc/");
    assert_eq!(filters.after, "v1/pool1abc/x");
}

/// Both filters are optional, and absent means "the whole archive from its
/// start" rather than nothing.
#[test]
fn a_query_with_no_filters_asks_for_everything() {
    let filters = Filters::parse("/v1/submissions").unwrap();
    assert_eq!(filters.prefix, "");
    assert_eq!(filters.after, "");
}

/// A key is percent-encoded by anything that builds URLs properly, and the
/// `/` in it is what a prefix filter is made of.
#[test]
fn a_percent_encoded_prefix_is_decoded() {
    let filters = Filters::parse("/v1/submissions?prefix=v1%2Fpool1abc%2F").unwrap();
    assert_eq!(filters.prefix, "v1/pool1abc/");
}

/// A lossy decode would answer a query the client never sent, and answer it
/// with a 200. `%ff` is no UTF-8 sequence.
#[test]
fn a_filter_whose_escapes_are_not_utf8_is_refused() {
    for field in ["prefix", "after"] {
        let error = Filters::parse(&format!("/v1/submissions?{field}=v1%2F%ff"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(field), "{field}: {error}");
    }
    let error = query_value("/v1/object?key=%ff", "key")
        .unwrap_err()
        .to_string();
    assert!(error.contains("key"), "got: {error}");
}

/// The index matches a prefix with GLOB, so a client that sent a GLOB
/// metacharacter would get a pattern match where it asked for a prefix. Refused
/// naming the character rather than silently widening the answer.
#[test]
fn a_prefix_holding_a_glob_metacharacter_is_refused() {
    for (prefix, character) in [("v1/*", '*'), ("v1/?", '?'), ("v1/[a-z]", '[')] {
        let error = Filters::parse(&format!("/v1/submissions?prefix={prefix}"))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(&format!("{character:?}")),
            "{prefix}: the refusal must name {character:?}, got {error}"
        );
    }
}

/// What `percent_decoded` does with an escape that is not two hex digits:
/// half-written, complete but non-hex, and nothing at all.
#[test]
fn a_malformed_escape_is_kept_literally() {
    for written in ["%zz", "%f", "%"] {
        let filters = Filters::parse(&format!("/v1/submissions?prefix=v1/{written}")).unwrap();
        assert_eq!(filters.prefix, format!("v1/{written}"));
    }
}

/// The shape a developer parses. The key is what the download route takes, so
/// it has to be in the answer.
#[test]
fn a_page_serializes_as_its_keys_and_what_they_encode() {
    let name = ObjectName {
        pool_id: pool_of(&test_key()),
        counter: 7,
        timestamp: test_now(),
    };
    let listing = Listing {
        objects: vec![name],
        truncated: true,
    };

    let json: serde_json::Value = serde_json::from_str(&page(&listing)).unwrap();

    assert_eq!(json["truncated"], true);
    let submission = &json["submissions"][0];
    assert_eq!(submission["key"], name.to_key());
    assert_eq!(submission["pool_id"], name.pool_id.to_bech32());
    assert_eq!(submission["counter"], 7);
    // RFC 3339, as the envelope stamps a timestamp (`metsuke_wire::envelope`).
    assert_eq!(submission["timestamp"], "2025-08-12T12:00:00Z");
}
