//! Why the submission route refused, as the fieldless reasons `http` answers
//! with and `metrics` counts. Its own module because both read it and neither
//! owns the other.

use crate::authority::Unauthorised;
use crate::intake::{IngestError, Rejection};

/// The enum, its label, its status and the list of every variant written
/// once, so a reason cannot be answered without a counter or counted without
/// a label.
macro_rules! refusals {
    ($($(#[$doc:meta])* $variant:ident => $label:literal, $status:literal,)*) => {
        /// Why this server refused a `POST` to the submission route, as the
        /// `reason` label spells it and with the status it answers. The
        /// fieldless side of `IngestError`, finer where one variant covers two
        /// reasons an operator acts on differently, plus a panic. Every refusal
        /// this server writes for that route lands on exactly one. What hyper
        /// refuses before `serve::handle` runs, a head over its size limit or
        /// one that never finishes, is a connection that failed before it was
        /// a submission, and is not counted.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Refusal {
            $($(#[$doc])* $variant,)*
        }

        impl Refusal {
            /// In declaration order, so `refusal as usize` indexes it.
            pub const ALL: &[Refusal] = &[$(Refusal::$variant,)*];

            pub fn label(self) -> &'static str {
                match self {
                    $(Refusal::$variant => $label,)*
                }
            }

            /// What the 4xx/5xx split means to the agent is ADR 0004; the
            /// finer codes are what an operator reads off the proxy log.
            pub fn status(self) -> u16 {
                match self {
                    $(Refusal::$variant => $status,)*
                }
            }
        }
    };
}

// Named as the `Rejection` each comes from, which says what each means; the
// four with no variant of their own say it here.
refusals! {
    Unattributed => "unattributed", 400,
    OversizedBody => "oversized", 413,
    BodyUnreadable => "body_unreadable", 400,
    BodyTimeout => "body_timeout", 408,
    NotASubmission => "not_a_submission", 400,
    UnknownPool => "not_allowlisted", 403,
    /// A Leios-key submission reached a server configured with no roster.
    NoRoster => "no_roster", 403,
    /// The roster lists no such Leios key for the pool claimed.
    LeiosKeyUnregistered => "leios_key_unregistered", 403,
    BadSignature => "bad_signature", 403,
    StaleTimestamp => "clock_skew", 400,
    RateLimited => "rate_limited", 429,
    ServerBusy => "server_busy", 429,
    UnreadableHeader => "header_unreadable", 400,
    NotItsProvenance => "not_its_provenance", 400,
    KeylessSchema => "keyless_schema", 400,
    /// The archive write failed: the server's fault.
    ArchiveUnavailable => "archive_unavailable", 503,
    /// Answering the submission panicked.
    Panicked => "panicked", 503,
}

impl From<&Rejection> for Refusal {
    fn from(rejection: &Rejection) -> Refusal {
        match rejection {
            Rejection::Unattributed(_) => Refusal::Unattributed,
            Rejection::OversizedBody { .. } => Refusal::OversizedBody,
            Rejection::BodyUnreadable(_) => Refusal::BodyUnreadable,
            Rejection::BodyTimeout { .. } => Refusal::BodyTimeout,
            Rejection::NotASubmission(_) => Refusal::NotASubmission,
            Rejection::UnknownPool { .. } => Refusal::UnknownPool,
            Rejection::RateLimited { .. } => Refusal::RateLimited,
            Rejection::ServerBusy { .. } => Refusal::ServerBusy,
            Rejection::BadSignature => Refusal::BadSignature,
            Rejection::Unauthorised(Unauthorised::NoRoster) => Refusal::NoRoster,
            Rejection::Unauthorised(Unauthorised::Unregistered { .. }) => {
                Refusal::LeiosKeyUnregistered
            }
            Rejection::StaleTimestamp { .. } => Refusal::StaleTimestamp,
            Rejection::UnreadableHeader(_) => Refusal::UnreadableHeader,
            Rejection::NotItsProvenance { .. } => Refusal::NotItsProvenance,
            Rejection::KeylessSchema { .. } => Refusal::KeylessSchema,
        }
    }
}

impl From<&IngestError> for Refusal {
    fn from(error: &IngestError) -> Refusal {
        match error {
            IngestError::Rejected(rejection) => Refusal::from(rejection),
            IngestError::Archive(_) => Refusal::ArchiveUnavailable,
        }
    }
}
