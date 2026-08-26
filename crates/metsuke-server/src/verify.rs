//! Re-verify a stored object from nothing but the object: its key, its
//! metadata and its bytes (ADR 0005). `audit` runs it over a whole bucket.
//!
//! Nothing here decompresses either. What ingest filed an object under is what
//! its signature and its header frame say, so that is what re-deriving the key
//! checks — the payload is the consumer's to read.

use metsuke_wire::envelope::{Header, read_header};

use crate::archive::{ArchiveError, Fetch, FetchedObject, Kind, List, ObjectName};
use crate::authority::Signed;

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{key}: the signature does not verify over the stored bytes")]
    BadSignature { key: String },
    #[error("{key}: the header frame does not read: {reason}")]
    UnreadableHeader { key: String, reason: String },
    #[error("{key}: nothing files a schema v{schema_version} batch")]
    UnnameableKind { key: String, schema_version: u32 },
    #[error("{key}: the batch inside belongs at {expected}")]
    Misfiled { key: String, expected: String },
}

/// Verify one stored object, yielding the header frame it carries.
///
/// The pool is derived from the stored key, never read off the object: an
/// object whose bytes another key signed re-derives a different pool and is
/// reported as misfiled.
pub fn verify(object: &FetchedObject, max_header_bytes: u64) -> Result<Header, VerifyError> {
    let key = object.name.to_key();
    let signed = Signed {
        vkey: object.vkey,
        signature: object.signature,
        wire_bytes: &object.wire_bytes,
    };
    if !signed.verifies() {
        return Err(VerifyError::BadSignature { key });
    }
    let header = read_header(&object.wire_bytes, max_header_bytes).map_err(|error| {
        VerifyError::UnreadableHeader {
            key: key.clone(),
            reason: error.to_string(),
        }
    })?;
    let kind = Kind::of(header.schema_version).ok_or(VerifyError::UnnameableKind {
        key: key.clone(),
        schema_version: header.schema_version,
    })?;
    // The key is `store`'s output, so re-deriving it checks the pool, the agent
    // and the kind it was filed under in one go. The id is the key's own, being
    // the one thing about an object that is not inside it.
    let expected = ObjectName {
        id: object.name.id,
        pool_id: signed.pool_id(),
        agent_id: header.agent_id.clone(),
        kind,
    }
    .to_key();
    if expected != key {
        return Err(VerifyError::Misfiled { key, expected });
    }
    Ok(header)
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
}

/// Fetch and re-verify every object in the archive.
///
/// A listing that cannot be read stops the audit — a short listing would
/// otherwise report a clean bucket it never looked at. One object that cannot
/// be read or does not verify stops nothing: the point is to find all of them.
pub fn audit(archive: &(impl List + Fetch), max_header_bytes: u64) -> Result<Audit, AuditError> {
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
            Ok(object) => match verify(&object, max_header_bytes) {
                Ok(_) => verified += 1,
                Err(error) => failures.push(error.into()),
            },
        }
    }
    Ok(Audit { verified, failures })
}
