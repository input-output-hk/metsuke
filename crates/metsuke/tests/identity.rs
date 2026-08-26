//! Who the agent says it is (ticket metsuke-jfb.3): the id it stamps every
//! line with, and the refusal when its key does not speak for the configured
//! pool.

use metsuke::identity::{self, IdentityError};
use metsuke_wire::envelope::{PoolId, SigningKey};

mod support;
use support::test_key;

#[test]
fn a_configured_agent_id_is_slugified() {
    let id = identity::agent_id(Some("Relay_1")).unwrap();
    assert_eq!(id.as_str(), "relay-1");
}

// Nothing to make an id out of is the one case that refuses, and it names what
// it was given.
#[test]
fn a_configured_agent_id_with_nothing_in_it_fails_loudly() {
    let error = identity::agent_id(Some("...")).unwrap_err();
    assert!(
        error.to_string().contains("..."),
        "the refusal must name what it was given, got: {error}"
    );
}

// No configured id means this machine's own name, which is a valid id whatever
// the machine is called.
#[test]
fn an_absent_agent_id_falls_back_to_this_machine() {
    let id = identity::agent_id(None).unwrap();
    assert!(!id.as_str().is_empty());
}

#[test]
fn the_pool_a_key_hashes_to_is_accepted() {
    let key = test_key();
    identity::check_pool_id(
        PoolId::from_cold_key(&key.verifying_key()),
        &key.verifying_key(),
    )
    .unwrap();
}

// A pool id that is not this key's hash is a configuration mistake the agent
// cannot ship past: every batch it sealed would be refused. The refusal names
// both, so the operator can see which one is wrong.
#[test]
fn a_pool_id_the_key_does_not_hash_to_is_refused() {
    let key = test_key();
    let other = PoolId::from_cold_key(&SigningKey::from_bytes(&[9u8; 32]).verifying_key());

    let error = identity::check_pool_id(other, &key.verifying_key()).unwrap_err();

    let IdentityError::PoolIdMismatch {
        configured,
        implied,
    } = &error
    else {
        panic!("expected a pool id mismatch, got {error:?}");
    };
    assert_eq!(*configured, other);
    assert_eq!(*implied, PoolId::from_cold_key(&key.verifying_key()));
    let message = error.to_string();
    assert!(
        message.contains(&other.to_bech32())
            && message.contains(&PoolId::from_cold_key(&key.verifying_key()).to_bech32()),
        "the refusal must name both pool ids, got: {message}"
    );
}
