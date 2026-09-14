use thiserror::Error;
use uuid::Uuid;

use crate::PluginId;

#[derive(Debug, Error)]
pub enum ProfileError {
    /// No profile exists with the requested UUID.
    #[error("profile {0} was not found")]
    NotFound(Uuid),
    /// A profile name is empty after trimming surrounding whitespace.
    #[error("profile name must not be blank")]
    InvalidName,
    /// The only remaining profile cannot be deleted.
    #[error("the last profile {0} cannot be deleted")]
    LastProfile(Uuid),
    /// No available provider supplies accounts for this storefront.
    #[error("storefront account provider {0} was not found")]
    ProviderNotFound(PluginId),
    /// The profile already has an account from this provider.
    #[error("profile {profile} already has an account from provider {provider}")]
    AccountAlreadyLinked { profile: Uuid, provider: PluginId },
    /// The profile has no account from this provider.
    #[error("profile {profile} has no account from provider {provider}")]
    AccountNotLinked { profile: Uuid, provider: PluginId },
    /// The provider rejected or failed an account operation.
    #[error("storefront account provider {provider}: {message}")]
    Provider { provider: PluginId, message: String },
}
