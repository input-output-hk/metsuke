//! Developer pull access (`metsuke_server::developer`): who may read the
//! archive, what a filter may ask for, and the JSON one page answers as.

use base64::Engine as _;
use metsuke_server::archive::{KEY_PREFIX, Page};
use metsuke_server::config::DeveloperConfig;
use metsuke_server::developer::{
    Accounts, Developer, Filters, LIST_MAX_ROWS_CAP, Username, page, query_value,
};

mod support;
use support::{
    DEVELOPER_PASSWORD, DEVELOPER_USER, OTHER_DEVELOPER_PASSWORD, OTHER_DEVELOPER_USER,
    developer_accounts, developer_config, developer_secret, nonzero_u32,
};

fn developer() -> Developer {
    let dir = tempfile::tempdir().unwrap();
    Developer::new(&developer_config(dir.path()), developer_accounts())
}

/// The `Authorization` header a client sends for these credentials.
fn basic(user: &str, password: &str) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
    format!("Basic {encoded}")
}

/// Every account in the secret authenticates, and the answer says which one it
/// was: that is what lets a log name the developer behind a pull.
#[test]
fn each_configured_account_is_authorized_as_itself() {
    for (user, password) in [
        (DEVELOPER_USER, DEVELOPER_PASSWORD),
        (OTHER_DEVELOPER_USER, OTHER_DEVELOPER_PASSWORD),
    ] {
        let header = basic(user, password);
        let developer = developer();
        let authorized = developer.authorize(Some(&header)).expect(user);
        assert_eq!(authorized.as_str(), user);
    }
}

/// One account's password does not open another's, which is the whole point of
/// a table rather than a shared secret.
#[test]
fn an_accounts_password_is_only_its_own() {
    let header = basic(DEVELOPER_USER, OTHER_DEVELOPER_PASSWORD);
    assert!(developer().authorize(Some(&header)).is_err());
}

/// A refusal names the account that was presented, where the client named one
/// a parse accepts, and nothing else about the credential.
#[test]
fn a_refusal_names_the_presented_account() {
    let header = basic(DEVELOPER_USER, "not the password");
    let refusal = developer()
        .authorize(Some(&header))
        .unwrap_err()
        .to_string();
    assert!(refusal.contains(DEVELOPER_USER), "got: {refusal}");
    assert!(
        !refusal.contains("not the password"),
        "a refusal must not carry the password: {refusal}"
    );
}

/// And a username the parse refuses is not repeated back into the log: it is
/// the one part of the header nothing has bounded.
#[test]
fn a_refusal_does_not_echo_an_unparseable_username() {
    let header = basic("Not A Username\nwith a line of its own", DEVELOPER_PASSWORD);
    let refusal = developer()
        .authorize(Some(&header))
        .unwrap_err()
        .to_string();
    assert!(!refusal.contains('\n'), "got: {refusal}");
    assert!(!refusal.contains("Not A Username"), "got: {refusal}");
}

/// The gate has to be closed by default: a request that says nothing about who
/// it is must not read the archive.
#[test]
fn a_request_without_an_authorization_header_is_refused() {
    assert!(developer().authorize(None).is_err());
}

#[test]
fn a_wrong_password_is_refused() {
    let header = basic(DEVELOPER_USER, "not the password");
    assert!(developer().authorize(Some(&header)).is_err());
}

#[test]
fn an_unconfigured_user_with_a_configured_password_is_refused() {
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
    let header = basic(DEVELOPER_USER, DEVELOPER_PASSWORD).replace("Basic", "basic");
    assert!(developer().authorize(Some(&header)).is_ok());
}

/// A password is whatever the TOML string holds, colons and all: RFC 7617
/// makes the first colon the separator, so the rest of the credential is the
/// password.
#[test]
fn a_password_holding_a_colon_authorizes() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::parse("dev = \"pass:with:colons\"").unwrap();
    let developer = Developer::new(&developer_config(dir.path()), accounts);

    let header = basic("dev", "pass:with:colons");

    assert!(developer.authorize(Some(&header)).is_ok());
}

/// The secret is TOML, so the trailing newline an editor adds is not part of
/// any password and nothing has to trim it (metsuke-jfb.32).
#[test]
fn a_trailing_newline_is_not_part_of_a_password() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::parse(&format!("{}\n\n", developer_secret())).unwrap();
    let developer = Developer::new(&developer_config(dir.path()), accounts);

    let header = basic(DEVELOPER_USER, DEVELOPER_PASSWORD);

    assert!(developer.authorize(Some(&header)).is_ok());
}

/// What a secret file may not hold. Each is a startup failure rather than a
/// server that answers on a credential nobody set, or none.
#[test]
fn a_secret_that_names_no_usable_account_is_refused() {
    for (written, expected) in [
        ("", "names no accounts"),
        ("dev = \"\"", "empty password"),
        ("\"dev user\" = \"password\"", "not a username"),
        ("\"dev:user\" = \"password\"", "not a username"),
        ("dev = 12", "does not parse"),
        ("[dev]\npassword = \"p\"", "does not parse"),
    ] {
        let error = Accounts::parse(written).expect_err(written).to_string();
        assert!(
            error.contains(expected),
            "{written:?} must fail naming {expected:?}, got: {error}"
        );
    }
}

/// A person's name is what an operator writes here, so case is part of the
/// username rather than something folded away, and it needs no quoting: the
/// alphabet is a TOML bare key's.
#[test]
fn a_username_is_a_persons_name_as_written() {
    let dir = tempfile::tempdir().unwrap();
    let accounts = Accounts::parse("JaneDoe = \"s3cret\"").expect("a bare key needs no quotes");
    let developer = Developer::new(&developer_config(dir.path()), accounts);

    assert!(
        developer
            .authorize(Some(&basic("JaneDoe", "s3cret")))
            .is_ok()
    );
    assert!(
        developer
            .authorize(Some(&basic("janedoe", "s3cret")))
            .is_err(),
        "the digest is over the bytes, so the case is part of the credential"
    );
}

/// The bound on a username is what keeps a refusal's log line bounded, and it
/// reads both sides the same way: an account too long to present is one too
/// long to configure.
#[test]
fn a_username_over_the_cap_is_refused_on_both_sides() {
    let long = "a".repeat(65);

    assert!(Username::parse(&long).is_err());
    assert!(
        Accounts::parse(&format!("{long} = \"password\""))
            .unwrap_err()
            .to_string()
            .contains("at most")
    );
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

    let developer = Developer::new(&asking_for_more, developer_accounts());

    assert_eq!(developer.list_max_rows(), LIST_MAX_ROWS_CAP);
}

/// And a bound under it is the operator's to choose.
#[test]
fn a_configured_row_bound_under_the_page_cap_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let developer = Developer::new(&developer_config(dir.path()), developer_accounts());

    assert_eq!(
        developer.list_max_rows(),
        developer_config(dir.path()).list_max_rows
    );
}
