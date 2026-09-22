use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum ProfileError {
    /// No profile exists with the requested UUID.
    #[error("profile {0} was not found")]
    NotFound(Uuid),
    /// The only remaining profile cannot be deleted.
    #[error("the last profile {0} cannot be deleted")]
    LastProfile(Uuid),
    /// No available provider supplies accounts for this storefront.
    #[error("storefront account provider {0} was not found")]
    ProviderNotFound(String),
    /// The profile already has an account from this provider.
    #[error("profile {profile} already has an account from provider {provider}")]
    AccountAlreadyLinked { profile: Uuid, provider: String },
    /// The profile has no account from this provider.
    #[error("profile {profile} has no account link {link}")]
    AccountNotLinked { profile: Uuid, link: Uuid },
    #[error("failed to clean credentials for account link {link_id}: {source}")]
    CredentialCleanup {
        link_id: Uuid,
        source: keyring::Error,
    },
    #[error("{source}; failed to clean credentials for account link {link_id}: {cleanup}")]
    AccountLinkRollback {
        link_id: Uuid,
        source: Box<crate::error::Error>,
        cleanup: keyring::Error,
    },
    /// The provider rejected or failed an account operation.
    #[error("storefront account provider {provider}: {message}")]
    Provider { provider: String, message: String },
}
