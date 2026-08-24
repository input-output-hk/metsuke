//! Re-verify a stored object from nothing but the object: its key, its
//! metadata and its bytes (ADR 0005). `audit` runs it over a whole bucket.

use metsuke_wire::envelope::{Envelope, PoolId, SCHEMA_VERSION};
use time::OffsetDateTime;

use crate::archive::{ArchiveError, Fetch, FetchedObject, List, ObjectName};
use crate::authority::{AuthError, Authority, Refusal, Signed, Undecided, authenticate};

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{key}: the stored key does not speak for pool {pool_id}: {refusal}")]
    UnauthorizedKey {
        key: String,
        pool_id: PoolId,
        refusal: Refusal,
    },
    /// Not a finding about the object: nothing was decided about it.
    #[error(transparent)]
    Undecided(#[from] Undecided),
    #[error("{key}: the signature does not verify over the stored bytes")]
    BadSignature { key: String },
    #[error("{key}: the payload is not a schema v{SCHEMA_VERSION} envelope: {reason}")]
    MalformedPayload { key: String, reason: String },
    #[error("{key}: the payload inflates past the {max} byte limit")]
    OversizedPayload { key: String, max: u64 },
    #[error("{key}: metadata says {field} is {stored}, the signed payload says {signed}")]
    Disagrees {
        key: String,
        field: &'static str,
        stored: String,
        signed: String,
    },
    #[error("{key}: the payload inside belongs at {expected}")]
    Misfiled { key: String, expected: String },
}

/// Verify one stored object, yielding the envelope it holds.
///
/// `authority` is ADR 0003's key check, the same one ingest ran: an object a
/// pool's Calidus key signed verifies only against an authority that can
/// resolve it.
///
/// The metadata is checked against the payload rather than trusted: a header
/// that disagrees with the signed bytes is unsigned, so the bytes win and the
/// disagreement is the finding.
pub fn verify(
    object: &FetchedObject,
    max_decompressed_bytes: u64,
    authority: &mut impl Authority,
    now: OffsetDateTime,
) -> Result<Envelope, VerifyError> {
    let key = object.name.to_key();
    let pool_id = object.name.pool_id;
    let signed = Signed {
        pool_id,
        vkey: object.vkey,
        signature: object.signature,
        wire_bytes: &object.wire_bytes,
    };
    let envelope = authenticate(authority, &signed, max_decompressed_bytes, now).map_err(
        |error| match error {
            AuthError::UnauthorizedKey { pool_id, refusal } => VerifyError::UnauthorizedKey {
                key: key.clone(),
                pool_id,
                refusal,
            },
            AuthError::BadSignature => VerifyError::BadSignature { key: key.clone() },
            AuthError::Undecided(error) => error.into(),
            AuthError::OversizedPayload { max } => VerifyError::OversizedPayload {
                key: key.clone(),
                max,
            },
            AuthError::MalformedPayload { reason } => VerifyError::MalformedPayload {
                key: key.clone(),
                reason,
            },
        },
    )?;
    // The key is `store`'s output, so re-deriving it from the payload checks
    // the pool, the counter and the timestamp it was filed under in one go.
    let expected = ObjectName {
        pool_id: envelope.pool_id,
        counter: envelope.counter,
        timestamp: envelope.timestamp,
    }
    .to_key();
    if expected != key {
        return Err(VerifyError::Misfiled { key, expected });
    }
    let disagrees = |field, stored: String, signed: String| VerifyError::Disagrees {
        key: key.clone(),
        field,
        stored,
        signed,
    };
    if envelope.counter != object.metadata_counter {
        return Err(disagrees(
            "counter",
            object.metadata_counter.to_string(),
            envelope.counter.to_string(),
        ));
    }
    if envelope.schema_version != object.metadata_schema_version {
        return Err(disagrees(
            "schema_version",
            object.metadata_schema_version.to_string(),
            envelope.schema_version.to_string(),
        ));
    }
    Ok(envelope)
}

/// What re-verifying a whole bucket found.
#[derive(Debug)]
pub struct Audit {
    pub verified: usize,
    pub failures: Vec<AuditFailure>,
}

/// Why an object is not counted as verified. The two are different findings:
/// one that failed verification is evidence about the corpus, one that could
/// not be read is evidence about nothing.
#[derive(Debug, thiserror::Error)]
pub enum AuditFailure {
    #[error("{key}: not read, so not checked: {reason}")]
    Unreadable { key: String, reason: String },
    #[error(transparent)]
    Failed(#[from] VerifyError),
}

impl Audit {
    pub fn unreadable(&self) -> usize {
        self.failures
            .iter()
            .filter(|failure| matches!(failure, AuditFailure::Unreadable { .. }))
            .count()
    }

    pub fn failed(&self) -> usize {
        self.failures.len() - self.unreadable()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Undecided(#[from] Undecided),
}

/// Fetch and re-verify every object in the archive.
///
/// A listing that cannot be read stops the audit — a short listing would
/// otherwise report a clean bucket it never looked at. So does a key check
/// that reached no answer: every Calidus pool's objects would be reported as
/// findings about the corpus when the finding is about the directory. One object
/// that cannot be read or does not verify stops nothing: the point is to find
/// all of them.
///
/// `now` is read once for the whole run, so one resolution stands for the whole
/// bucket however long it takes, whatever the TTL says.
pub fn audit(
    archive: &(impl List + Fetch),
    max_decompressed_bytes: u64,
    authority: &mut impl Authority,
    now: OffsetDateTime,
) -> Result<Audit, AuditError> {
    let mut verified = 0;
    let mut failures = Vec::new();
    // The whole listing first: fetching inside the visitor would hold the
    // listing open for as long as re-verifying the bucket takes.
    for key in archive.keys()? {
        match archive.fetch(&key) {
            Err(error) => failures.push(AuditFailure::Unreadable {
                key,
                reason: error.to_string(),
            }),
            Ok(object) => match verify(&object, max_decompressed_bytes, authority, now) {
                Ok(_) => verified += 1,
                Err(VerifyError::Undecided(error)) => return Err(error.into()),
                Err(error) => failures.push(error.into()),
            },
        }
    }
    Ok(Audit { verified, failures })
}
