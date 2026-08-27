//! Developer pull access (`metsuke_server::developer`): who may read the
//! archive, what a filter may ask for, and the JSON one page answers as.

use base64::Engine as _;
use metsuke_server::archive::{KEY_PREFIX, Page};
use metsuke_server::config::DeveloperConfig;
use metsuke_server::developer::{Developer, Filters, LIST_MAX_ROWS_CAP, page, query_value};

mod support;
use support::{DEVELOPER_PASSWORD, developer_config, nonzero_u32};

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
/// start" rather than nothing. The whole archive is what this server filed,
/// so the prefix an absent one becomes is the object keys' own.
#[test]
fn a_query_with_no_filters_asks_for_everything() {
    let filters = Filters::parse("/v1/submissions").unwrap();
    assert_eq!(filters.prefix, KEY_PREFIX);
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

/// What `percent_decoded` does with an escape that is not two hex digits:
/// half-written, complete but non-hex, and nothing at all.
#[test]
fn a_malformed_escape_is_kept_literally() {
    for written in ["%zz", "%f", "%"] {
        let filters = Filters::parse(&format!("/v1/submissions?prefix=v1/{written}")).unwrap();
        assert_eq!(filters.prefix, format!("v1/{written}"));
    }
}

/// The shape a developer parses: the keys as the archive handed them over,
/// unparsed, and whether the bound cut the page off.
#[test]
fn a_page_serializes_as_the_keys_the_archive_listed() {
    let listing = Page {
        keys: vec!["v1/2026-08-26/one.jsonl.zst".to_string()],
        truncated: true,
    };

    let keys = listing.keys.clone();
    let json: serde_json::Value = serde_json::from_str(&page(listing)).unwrap();

    assert_eq!(json["truncated"], true);
    assert_eq!(json["keys"][0], keys[0]);
}

/// One listing request is one upstream request, so a configured bound above
/// what a page can hold is clamped rather than reporting a page as the whole
/// archive.
#[test]
fn a_configured_row_bound_over_the_page_cap_is_clamped_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let asking_for_more = DeveloperConfig {
        list_max_rows: nonzero_u32(LIST_MAX_ROWS_CAP.get() + 1),
        ..developer_config(dir.path())
    };

    let developer = Developer::new(&asking_for_more, DEVELOPER_PASSWORD);

    assert_eq!(developer.list_max_rows(), LIST_MAX_ROWS_CAP);
}

/// And a bound under it is the operator's to choose.
#[test]
fn a_configured_row_bound_under_the_page_cap_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let developer = Developer::new(&developer_config(dir.path()), DEVELOPER_PASSWORD);

    assert_eq!(
        developer.list_max_rows(),
        developer_config(dir.path()).list_max_rows
    );
}
