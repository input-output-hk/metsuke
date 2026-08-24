//! The gate: what the two halves must agree on before a pool is allowlisted,
//! and that what the command emits is the config the server reads.

use metsuke_server::applications::{
    ApplicationCode, Codes, Excluded, Registered, gate, read_codes, read_registered,
};
use metsuke_server::config::ServerConfig;
use metsuke_wire::envelope::PoolId;

use std::collections::{BTreeMap, BTreeSet};

mod support;
use support::{
    allowlist_toml, calidus_key, example_config, other_key, pool_of, rotated_calidus_key, test_key,
};

fn code(text: &str) -> ApplicationCode {
    ApplicationCode::parse(text).expect("a test code is well formed")
}

/// One half of the gate, as either side hands it over.
fn codes(rows: &[(PoolId, &str)]) -> Codes {
    rows.iter()
        .map(|(pool_id, text)| (*pool_id, code(text)))
        .collect()
}

/// The registered half as a clean answer: every row read, no pool contradicted.
fn registered(rows: &[(PoolId, &str)]) -> Registered {
    Registered {
        codes: codes(rows),
        unreadable: 0,
        contradicted: BTreeSet::new(),
    }
}

/// An applications export, with the two columns the gate reads and one it does
/// not.
fn applications_csv(rows: &[(PoolId, &str)]) -> String {
    let mut text = String::from("submitted_at,pool_id,application_code\n");
    for (pool_id, application_code) in rows {
        text.push_str(&format!(
            "2026-08-01T00:00:00Z,{pool_id},{application_code}\n"
        ));
    }
    text
}

#[test]
fn a_pool_whose_code_is_its_registered_one_is_allowlisted() {
    let pool = pool_of(&test_key());
    let found = gate(
        codes(&[(pool, "MUSA-0001")]),
        registered(&[(pool, "MUSA-0001")]),
    );
    assert_eq!(found.allowed, codes(&[(pool, "MUSA-0001")]));
    assert!(found.excluded.is_empty(), "got: {:?}", found.excluded);
    assert_eq!(found.did_not_apply, 0);
}

/// The two halves are one gate: each alone allowlists nobody, whichever side
/// is missing.
#[test]
fn a_pool_on_only_one_side_is_excluded() {
    let applicant = pool_of(&test_key());
    let other = pool_of(&other_key());
    let found = gate(
        codes(&[(applicant, "MUSA-0001")]),
        registered(&[(other, "MUSA-0002")]),
    );
    assert!(found.allowed.is_empty(), "got: {:?}", found.allowed);
    assert_eq!(
        found.excluded,
        BTreeMap::from([(applicant, Excluded::NotRegistered)])
    );
    assert_eq!(found.did_not_apply, 1);
}

#[test]
fn a_code_that_is_not_the_registered_one_is_excluded() {
    let pool = pool_of(&test_key());
    let found = gate(
        codes(&[(pool, "MUSA-0001")]),
        registered(&[(pool, "MUSA-0002")]),
    );
    assert!(found.allowed.is_empty(), "got: {:?}", found.allowed);
    assert_eq!(
        found.excluded,
        BTreeMap::from([(
            pool,
            Excluded::CodeMismatch {
                registered: code("MUSA-0002")
            }
        )])
    );
}

#[test]
fn an_applicant_the_registered_half_contradicts_is_not_reported_as_unregistered() {
    let pool = pool_of(&test_key());
    let found = gate(
        codes(&[(pool, "MUSA-0001")]),
        Registered {
            codes: Codes::new(),
            unreadable: 0,
            contradicted: BTreeSet::from([pool]),
        },
    );
    assert!(found.allowed.is_empty(), "got: {:?}", found.allowed);
    assert_eq!(
        found.excluded,
        BTreeMap::from([(pool, Excluded::ContradictoryCodes)])
    );
}

#[test]
fn a_pool_named_twice_refuses_the_whole_file() {
    let pool = pool_of(&test_key());
    let text = applications_csv(&[(pool, "MUSA-0001"), (pool, "MUSA-0002")]);
    let error = read_codes(&text).unwrap_err().to_string();
    assert!(error.contains(&pool.to_bech32()), "got: {error}");
    assert!(error.contains("row 3"), "got: {error}");
}

#[test]
fn the_two_columns_it_names_are_the_two_it_reads() {
    let pool = pool_of(&test_key());
    let other = pool_of(&other_key());
    let rows = read_codes(&applications_csv(&[
        (pool, "MUSA-0001"),
        (other, "MUSA-0002"),
    ]))
    .unwrap();
    assert_eq!(rows, codes(&[(pool, "MUSA-0001"), (other, "MUSA-0002")]));
}

#[test]
fn a_quoted_column_the_gate_ignores_does_not_shift_the_ones_it_reads() {
    let pool = pool_of(&test_key());
    let text = format!(
        "operator,pool_id,application_code\n\"Example Pools, Ltd\",{pool},MUSA-0001\n",
        pool = pool.to_bech32(),
    );
    assert_eq!(read_codes(&text).unwrap(), codes(&[(pool, "MUSA-0001")]));
}

#[test]
fn a_file_without_the_columns_the_gate_reads_is_refused() {
    let error = read_codes("pool,code\npool1abc,MUSA-0001\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("application_code"), "got: {error}");
}

#[test]
fn a_pool_id_that_is_not_bech32_is_refused_naming_its_row() {
    let error = read_codes("pool_id,application_code\npool1nope,MUSA-0001\n")
        .unwrap_err()
        .to_string();
    assert!(error.contains("row 2"), "got: {error}");
}

/// The query's answer as `read_registered` takes it: a pool's bech32 view, and
/// the JSON value under the key, which `->>` renders as NULL where it is no
/// string.
fn registered_rows(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    rows.iter()
        .map(|(pool_id, code)| (pool_id.to_string(), code.map(str::to_string)))
        .collect()
}

#[test]
fn a_registered_row_that_does_not_read_is_counted_not_fatal() {
    let pool = pool_of(&test_key());
    let stranger = pool_of(&other_key());
    let found = read_registered(registered_rows(&[
        (&pool.to_string(), Some("MUSA-0001")),
        (&stranger.to_string(), Some("hello world")),
        ("not-a-pool-id", Some("MUSA-0002")),
    ]));
    assert_eq!(
        found,
        Registered {
            codes: codes(&[(pool, "MUSA-0001")]),
            unreadable: 2,
            contradicted: BTreeSet::new(),
        }
    );
}

#[test]
fn a_pool_the_registered_half_gives_two_codes_names_neither() {
    let pool = pool_of(&test_key());
    let other = pool_of(&other_key());
    let found = read_registered(registered_rows(&[
        (&pool.to_string(), Some("MUSA-0001")),
        (&pool.to_string(), Some("MUSA-0002")),
        (&other.to_string(), Some("MUSA-0003")),
    ]));
    assert_eq!(
        found,
        Registered {
            codes: codes(&[(other, "MUSA-0003")]),
            unreadable: 0,
            contradicted: BTreeSet::from([pool]),
        }
    );
}

/// A repeated identical row must not cost the pool its place.
#[test]
fn the_same_registered_row_twice_still_names_its_code() {
    let pool = pool_of(&test_key());
    let found = read_registered(registered_rows(&[
        (&pool.to_string(), Some("MUSA-0001")),
        (&pool.to_string(), Some("MUSA-0001")),
    ]));
    assert_eq!(
        found,
        Registered {
            codes: codes(&[(pool, "MUSA-0001")]),
            unreadable: 0,
            contradicted: BTreeSet::new(),
        }
    );
}

/// The label holds whatever the operator posted, so the key may carry an object
/// or an array, which `->>` hands back as NULL. That is a stranger's row to
/// count, not an answer to fail on.
#[test]
fn a_registered_row_whose_code_is_no_string_is_counted_not_fatal() {
    let pool = pool_of(&test_key());
    let other = pool_of(&other_key());
    let found = read_registered(registered_rows(&[
        (&pool.to_string(), None),
        (&other.to_string(), Some("MUSA-0001")),
    ]));
    assert_eq!(
        found,
        Registered {
            codes: codes(&[(other, "MUSA-0001")]),
            unreadable: 1,
            contradicted: BTreeSet::new(),
        }
    );
}

#[test]
fn an_empty_code_is_refused() {
    assert!(ApplicationCode::parse("").is_err());
    assert!(ApplicationCode::parse("  ").is_err());
}

#[test]
fn a_code_is_matched_without_its_surrounding_whitespace() {
    assert_eq!(code(" MUSA-0001 "), code("MUSA-0001"));
}

#[test]
fn a_code_outside_the_identifier_alphabet_is_refused() {
    for text in [
        "MUSA 0001",
        "MUSA\"0001",
        "MUSA\\0001",
        "MUSA\n0001",
        "MUSÄ",
    ] {
        assert!(
            ApplicationCode::parse(text).is_err(),
            "{text:?} must not parse"
        );
    }
}

/// The refusal `generate-allowlist` exits nonzero on. Both halves naming pools
/// is not enough: a gate that matched none of them emits an empty document,
/// which reads as a program nobody joined and would stop every upload.
#[test]
fn a_gate_that_allowlists_nobody_says_so() {
    let applicant = pool_of(&test_key());
    let stranger = pool_of(&other_key());

    assert!(
        gate(
            codes(&[(applicant, "MUSA-0001")]),
            registered(&[(stranger, "MUSA-0001")]),
        )
        .allowlists_nobody()
    );
    assert!(
        gate(
            codes(&[(applicant, "MUSA-0001")]),
            registered(&[(applicant, "MUSA-0002")]),
        )
        .allowlists_nobody(),
        "a mismatched code allowlists nobody either"
    );
    assert!(
        !gate(
            codes(&[(applicant, "MUSA-0001")]),
            registered(&[(applicant, "MUSA-0001")]),
        )
        .allowlists_nobody()
    );
}

/// Acceptance: what the command writes is the value the server's own parser
/// reads for `ingest.allowlist`, with no operator editing between them.
#[test]
fn the_emitted_pairs_load_as_the_servers_allowlist() {
    let first = pool_of(&test_key());
    let second = pool_of(&other_key());
    let both = [(first, "MUSA-0001"), (second, "MUSA-0002")];
    let found = gate(codes(&both), registered(&both));

    // The example's own one-pool allowlist out, the emitted pairs in as the
    // table they are: a `[ingest.allowlist]` header has to come after the rest
    // of `[ingest]`, which is where an operator's config puts it too.
    let emitted = found.to_toml();
    let spliced = format!(
        "{example}\n[ingest.allowlist]\n{emitted}",
        example = example_config().replace(
            &format!("allowlist = {}\n", allowlist_toml(&[pool_of(&test_key())])),
            "",
        ),
    );
    let config = ServerConfig::from_toml(&spliced).unwrap();
    assert_eq!(config.ingest.allowlist, found.allowed);
}

/// The artifact has to be a whole TOML document on its own, not a fragment that
/// only parses where it is pasted.
#[test]
fn the_emitted_pairs_are_a_toml_document_of_their_own() {
    let pool = pool_of(&test_key());
    let found = gate(
        codes(&[(pool, "MUSA-0001")]),
        registered(&[(pool, "MUSA-0001")]),
    );
    let parsed: Codes = toml::from_str(&found.to_toml()).unwrap();
    assert_eq!(parsed, found.allowed);
}

/// The same two halves produce the same bytes however the rows were ordered.
#[test]
fn the_emitted_pairs_do_not_depend_on_the_order_the_rows_arrive_in() {
    let pools = [
        pool_of(&test_key()),
        pool_of(&other_key()),
        pool_of(&calidus_key()),
        pool_of(&rotated_calidus_key()),
    ];
    let forwards: Vec<(PoolId, &str)> = pools.iter().map(|pool| (*pool, "MUSA-0001")).collect();
    let backwards: Vec<(PoolId, &str)> = forwards.iter().rev().copied().collect();

    let emitted = gate(codes(&forwards), registered(&forwards)).to_toml();
    assert_eq!(
        emitted,
        gate(codes(&backwards), registered(&backwards)).to_toml()
    );
    assert_eq!(emitted.lines().count(), 4);
}
