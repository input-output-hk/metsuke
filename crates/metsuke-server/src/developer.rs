//! Developer pull access to the archive (ticket metsuke-4zo.10): an account
//! per developer, a listing off the index, and objects handed back
//! untransformed (`archive::Bytes`).
//!
//! The accounts are the secret's contents rather than the config's, so what an
//! operator publishes names none of them and revoking one person is an edit to
//! one line of one encrypted file. What a credential decides is who may pull
//! the corpus, not whether it can be trusted: the bytes it guards are
//! self-verifying.

use std::collections::BTreeMap;

use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest as _};
use metsuke_wire::http::{AFTER_FIELD, Listing, PREFIX_FIELD};

use base64::Engine as _;

use crate::archive::{KEY_PREFIX, Page};
use crate::config::DeveloperConfig;

/// The realm a 401 names. Fixed: an operator changing it would only change
/// what a browser's prompt says.
pub const REALM: &str = "metsuke archive";

/// Keys one ListObjectsV2 answers with, which is S3's cap and so this
/// server's. A listing request is one upstream request, so a bound above it
/// would report a page as the whole archive.
pub const LIST_MAX_ROWS_CAP: std::num::NonZeroU32 = std::num::NonZeroU32::new(1000).unwrap();

/// How long a username may be. It bounds what a refusal can put in the
/// journal, and it refuses no credential that would have worked: an account
/// this long cannot be configured either, because the same parse reads both
/// sides.
const USERNAME_MAX_CHARS: usize = 64;

/// A developer account's name: letters, digits, `-` and `_`, which is exactly
/// what a TOML bare key holds, so no name an operator picks has to be quoted
/// in the secret. A person's name is what goes here, so case is kept, and it
/// is matched byte for byte because the digest is over the bytes.
///
/// Parsed rather than folded, and this is why the alphabet is bounded at all:
/// a username arrives in a header from anyone, and what a refusal puts in the
/// journal has to be something a parse already accepted. It excludes `:`,
/// which RFC 7617 makes the separator, so no account can be named something a
/// Basic header could not carry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Username(String);

#[derive(Debug, thiserror::Error)]
pub enum UsernameError {
    #[error("a username cannot be empty")]
    Empty,
    #[error("{found:?} is not a username: letters, digits, '-' and '_' only")]
    NotAUsername { found: String },
    #[error("a username is at most {USERNAME_MAX_CHARS} characters, and this one is {found}")]
    TooLong { found: usize },
}

impl Username {
    pub fn parse(text: &str) -> Result<Username, UsernameError> {
        if text.is_empty() {
            return Err(UsernameError::Empty);
        }
        if text.chars().count() > USERNAME_MAX_CHARS {
            return Err(UsernameError::TooLong {
                found: text.chars().count(),
            });
        }
        let named = text
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'));
        match named {
            true => Ok(Username(text.to_string())),
            false => Err(UsernameError::NotAUsername {
                found: text.to_string(),
            }),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Username {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Every account the secret names, each as the digest of the credential it
/// presents. The passwords are not held: a heap dump of a serving process
/// carries none of them.
pub struct Accounts(BTreeMap<[u8; 32], Username>);

impl std::fmt::Debug for Accounts {
    /// The count alone. A digest is what this server compares a presented
    /// credential against, so it is not a thing to format anywhere.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Accounts({})", self.0.len())
    }
}

/// Why a secret file names no usable set of accounts. Each is a startup
/// failure: the routes would otherwise be open on something nobody set, or
/// closed to everybody the operator meant to let in.
#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error("does not parse as a table of username to password: {0}")]
    NotATable(#[from] toml::de::Error),
    #[error("names no accounts, so nothing could read the archive")]
    Empty,
    #[error("names an unusable account: {source}")]
    Username {
        #[source]
        source: UsernameError,
    },
    #[error("gives {user} an empty password, which would authorize anyone naming that user")]
    EmptyPassword { user: Username },
}

impl Accounts {
    /// The secret as it is written: one `user = "password"` line each. A TOML
    /// string is exact, so nothing here trims what an operator quoted.
    pub fn parse(text: &str) -> Result<Accounts, AccountsError> {
        let written: BTreeMap<String, String> = toml::from_str(text)?;
        if written.is_empty() {
            return Err(AccountsError::Empty);
        }
        let mut accounts = BTreeMap::new();
        for (user, password) in written {
            let user =
                Username::parse(&user).map_err(|source| AccountsError::Username { source })?;
            if password.is_empty() {
                return Err(AccountsError::EmptyPassword { user });
            }
            accounts.insert(digest(&format!("{user}:{password}")), user);
        }
        Ok(Accounts(accounts))
    }

    /// How many accounts may pull, for the operator's startup line. The names
    /// are the secret's, so a count is all a log may say about them. Never
    /// zero, which is what `AccountsError::Empty` refuses.
    pub fn count(&self) -> usize {
        self.0.len()
    }
}

/// A request that may not read the archive. The reason is for the operator's
/// log and nothing else. Told which half of the credential was wrong, a
/// client learns whether the user exists, so the caller that answers is what
/// keeps it out of the body (`http::challenge`).
#[derive(Debug, thiserror::Error)]
pub struct Unauthorized {
    reason: &'static str,
    /// The account the client named, where it named one a parse accepts. A
    /// refusal carrying none is one nothing could attribute.
    presented: Option<Username>,
}

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.presented {
            Some(user) => write!(formatter, "{}, presented as {user}", self.reason),
            None => formatter.write_str(self.reason),
        }
    }
}

impl Unauthorized {
    fn anonymous(reason: &'static str) -> Unauthorized {
        Unauthorized {
            reason,
            presented: None,
        }
    }
}

/// Blake2b-256 over `user:password`, which is the byte string Basic auth
/// carries. Comparing digests rather than the secrets is what keeps the
/// compare's timing independent of how much of the password matched, without a
/// constant-time-compare dependency: the bytes it does compare are a hash's,
/// and reveal nothing about their input.
fn digest(credential: &str) -> [u8; 32] {
    let mut hash = Blake2b::<U32>::new();
    hash.update(credential.as_bytes());
    hash.finalize().into()
}

/// The accounts developer pulls authenticate as, and the listing bound they
/// share.
pub struct Developer {
    accounts: Accounts,
    list_max_rows: std::num::NonZeroU32,
}

impl Developer {
    pub fn new(config: &DeveloperConfig, accounts: Accounts) -> Developer {
        Developer {
            accounts,
            list_max_rows: config.list_max_rows,
        }
    }

    /// Keys one listing answers with, never more than one upstream page.
    /// Clamped rather than refused at load: `LIST_MAX_ROWS_CAP` is the
    /// protocol's, not the operator's, and a config that asks for more is
    /// asking for the most a page can hold.
    pub fn list_max_rows(&self) -> std::num::NonZeroU32 {
        self.list_max_rows.min(LIST_MAX_ROWS_CAP)
    }

    /// Which account `authorization` presents the credential of. Everything
    /// malformed is refused exactly as a wrong credential is, and differs only
    /// in what the refusal says to the log.
    pub fn authorize(&self, authorization: Option<&str>) -> Result<&Username, Unauthorized> {
        let presented = basic_credential(
            authorization.ok_or(Unauthorized::anonymous("no authorization header"))?,
        )?;
        self.accounts
            .0
            .get(&digest(&presented.credential))
            .ok_or(Unauthorized {
                reason: "the credential is not an account's",
                presented: Some(presented.user),
            })
    }
}

/// What a Basic header presents: the account it names, and the `user:password`
/// a digest is taken over.
struct Presented {
    user: Username,
    credential: String,
}

/// The credential a Basic header carries, or `Unauthorized` for anything that
/// is not one. A credential with no colon in it is not a credential: RFC 7617
/// makes the first colon the separator, so its absence means the client sent
/// something else.
///
/// A username the parse refuses is not repeated back to the log. It is the one
/// part of a request this layer reads that nothing has bounded yet.
fn basic_credential(authorization: &str) -> Result<Presented, Unauthorized> {
    let (scheme, encoded) = authorization
        .split_once(' ')
        .ok_or(Unauthorized::anonymous(
            "the authorization header names no scheme",
        ))?;
    if !scheme.eq_ignore_ascii_case("Basic") {
        return Err(Unauthorized::anonymous(
            "the authorization scheme is not Basic",
        ));
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Unauthorized::anonymous("the Basic credential is not base64"))?;
    let credential = String::from_utf8(decoded)
        .map_err(|_| Unauthorized::anonymous("the Basic credential is not UTF-8"))?;
    let (user, _) = credential.split_once(':').ok_or(Unauthorized::anonymous(
        "the Basic credential holds no colon",
    ))?;
    let user = Username::parse(user)
        .map_err(|_| Unauthorized::anonymous("the Basic credential names no username"))?;
    Ok(Presented { user, credential })
}

/// The two filters a listing request may carry.
#[derive(Debug)]
pub struct Filters {
    /// The literal head of an object key, which is what makes it a
    /// day-and-pool filter (`archive::ObjectName`).
    pub prefix: String,
    /// The last key of the previous page. Empty is the archive's start.
    pub after: String,
}

/// A query this layer will not act on.
#[derive(Debug, thiserror::Error)]
pub enum BadQuery {
    #[error("{field} is not UTF-8 once its percent escapes are decoded")]
    NotUtf8 { field: &'static str },
}

/// One query field of a request URL, percent-decoded, empty when absent. The
/// `/` an object key is made of is what a client that builds URLs properly
/// encodes, so decoding is not optional.
pub fn query_value(url: &str, field: &'static str) -> Result<String, BadQuery> {
    let Some(raw) = url
        .split_once('?')
        .map(|(_, query)| query)
        .unwrap_or("")
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == field)
        .map(|(_, value)| value)
    else {
        return Ok(String::new());
    };
    percent_decoded(raw).ok_or(BadQuery::NotUtf8 { field })
}

impl Filters {
    /// Read the filters off a request URL. An absent prefix becomes the
    /// archive's own, so "everything" is every object this server filed
    /// rather than every key in the bucket.
    pub fn parse(url: &str) -> Result<Filters, BadQuery> {
        let asked = query_value(url, PREFIX_FIELD)?;
        Ok(Filters {
            prefix: match asked.is_empty() {
                true => KEY_PREFIX.to_string(),
                false => asked,
            },
            after: query_value(url, AFTER_FIELD)?,
        })
    }
}

/// `%XX` decoded, or `None` when the bytes that come out are not UTF-8.
/// Refused rather than replaced: a lossy decode would answer a query the
/// client did not send, and answer it with a 200.
///
/// A malformed escape, half-written or complete but not hex, stays as
/// written. It cannot be part of an object key either way, so it fails at the
/// filter rather than silently becoming a different string.
pub fn percent_decoded(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let escape = bytes
            .get(index + 1..index + 3)
            .filter(|_| bytes[index] == b'%')
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match escape {
            Some(byte) => {
                decoded.push(byte);
                index += 3;
            }
            None => {
                decoded.push(bytes[index]);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

/// One page of the archive as the JSON a developer parses
/// (`metsuke_wire::http::Listing`).
pub fn page(listing: Page) -> String {
    let page = Listing {
        keys: listing.keys,
        truncated: listing.truncated,
    };
    serde_json::to_string(&page).expect("a page of keys and a flag serializes")
}
