use thiserror::Error;
use uuid::{NonNilUuid, Uuid};

#[derive(Debug, Error)]
pub enum ProfileError {
    /// No profile exists with the requested UUID.
    #[error("profile {0} was not found")]
    NotFound(Uuid),
    /// A profile name is empty after trimming surrounding whitespace.
    #[error("profile name must not be blank")]
    InvalidName,
    /// The selected profile cannot be deleted.
    #[error("selected profile {0} cannot be deleted")]
    Selected(Uuid),
    /// No loaded plugin provides accounts for this storefront.
    #[error("storefront account provider {0:?} was not found")]
    ProviderNotFound(NonNilUuid),
    /// The profile already has an account from this provider.
    #[error("profile {profile} already has an account from provider {provider:?}")]
    AccountAlreadyLinked { profile: Uuid, provider: NonNilUuid },
    /// The profile has no account from this provider.
    #[error("profile {profile} has no account from provider {provider:?}")]
    AccountNotLinked { profile: Uuid, provider: NonNilUuid },
    /// The provider rejected or failed an account operation.
    #[error("storefront account provider {provider:?}: {message}")]
    Provider {
        provider: NonNilUuid,
        message: String,
    },
    /// Another profile already owns the same provider account.
    #[error("account {account_id} from provider {provider} is already linked to profile {profile}")]
    AccountIdentityAlreadyLinked {
        profile: Uuid,
        provider: NonNilUuid,
        account_id: String,
    },
    /// Built-in providers cannot be replaced or removed by extensions.
    #[error("storefront account provider {0} is built in")]
    ProviderBuiltIn(NonNilUuid),
}
