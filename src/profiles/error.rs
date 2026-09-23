//! Errors specific to profile and account-link operations.

use thiserror::Error;
use uuid::Uuid;

/// A profile or linked-account invariant could not be satisfied.
///
/// # Examples
///
/// ```
/// use bottles_core::ProfileError;
///
/// let error = ProfileError::NotFound(Default::default());
/// assert!(matches!(error, ProfileError::NotFound(_)));
/// ```
#[derive(Debug, Error)]
pub enum ProfileError {
    /// The requested profile UUID is not present.
    #[error("profile {0} was not found")]
    NotFound(Uuid),
    /// Deletion was refused because the profile is the only one remaining.
    #[error("the last profile {0} cannot be deleted")]
    LastProfile(Uuid),
    /// The profile already contains a link for the requested provider.
    #[error("profile {profile} already has an account from provider {provider}")]
    AccountAlreadyLinked {
        /// Profile on which the duplicate was detected.
        profile: Uuid,
        /// Stable identifier of the already-linked provider.
        provider: String,
    },
    /// The account link was removed, but its credential could not be deleted.
    #[error("failed to clean credentials for account link {link_id}: {source}")]
    CredentialCleanup {
        /// Link whose credential could not be removed.
        link_id: Uuid,
        /// Credential-store failure.
        source: keyring::Error,
    },
    /// Persisting a new link failed and its newly stored credential could not
    /// be rolled back.
    #[error("{source}; failed to clean credentials for account link {link_id}: {cleanup}")]
    AccountLinkRollback {
        /// Link whose newly stored credential could not be rolled back.
        link_id: Uuid,
        /// Failure that triggered rollback.
        source: Box<crate::error::Error>,
        /// Credential-store failure encountered during rollback.
        cleanup: keyring::Error,
    },
    /// An account provider rejected or failed the linking operation.
    #[error("storefront account provider {provider}: {message}")]
    Provider {
        /// Stable identifier of the failing provider.
        provider: String,
        /// Provider-supplied diagnostic message.
        message: String,
    },
}
