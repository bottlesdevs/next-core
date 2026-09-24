//! Account-provider discovery and profile-link lifecycle management.
//!
//! Account metadata is persisted with the profile, while provider credentials
//! are kept in the platform credential store and keyed by link UUID.

use super::providers::LinkedAccount;
use super::{
    AccountLink, AccountLinkInteraction, AccountProviderInfo, ProfileError, Profiles,
    ProfilesState, credentials, providers,
};
use crate::{
    Operation,
    error::{Error, Result},
};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct CancellableInteraction {
    inner: Arc<dyn AccountLinkInteraction>,
    cancellation: CancellationToken,
}

#[async_trait::async_trait]
impl AccountLinkInteraction for CancellableInteraction {
    async fn request_input(
        &self,
        url: url::Url,
        instructions: String,
    ) -> std::result::Result<String, String> {
        self.cancellation
            .run_until_cancelled(self.inner.request_input(url, instructions))
            .await
            .ok_or_else(|| "account linking cancelled".to_owned())?
    }
}

impl Profiles {
    /// Returns the built-in and installed-plugin account providers.
    ///
    /// The built-in Steam provider appears first. A plugin that uses the
    /// reserved `steam` identifier is omitted.
    pub fn account_providers(&self) -> Vec<AccountProviderInfo> {
        providers::list(&self.inner.plugins)
    }

    /// Starts linking an account provider to a profile.
    ///
    /// The returned [`Operation`] must be driven by the caller. It validates
    /// the profile and provider membership, asks the provider to identify or
    /// authenticate an account, stores any returned credential, and finally
    /// publishes the new [`AccountLink`]. A profile can have at most one link
    /// for each provider.
    ///
    /// Cancellation is cooperative. Pending interaction requests are cancelled,
    /// but a provider call already accepted by a plugin may finish in the background
    /// after the operation is dropped. Once the write lock is acquired and
    /// credential persistence begins, the operation completes publication or
    /// rollback even if cancellation is requested.
    ///
    /// # Errors
    ///
    /// When driven, the operation returns [`ProfileError::NotFound`] for an
    /// unknown profile, [`ProfileError::AccountAlreadyLinked`] for a duplicate
    /// provider, or [`ProfileError::Provider`] when the provider rejects the
    /// request. Plugin loading, credential storage, persistence, cancellation,
    /// and rollback failures are also returned. Cancellation is reported as
    /// [`Error::Cancelled`] only before the persistence boundary described above.
    pub fn link_account(
        &self,
        profile_id: Uuid,
        provider_id: String,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Operation<AccountLink> {
        let profiles = self.clone();
        Operation::new(move |_, cancellation| async move {
            validate_account_link(&profiles.state(), profile_id, &provider_id)?;
            let provider = cancellation
                .run_until_cancelled(providers::get(&profiles.inner.plugins, &provider_id))
                .await
                .ok_or(Error::Cancelled)??;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let linked = provider
                .link_account(Arc::new(CancellableInteraction {
                    inner: interaction,
                    cancellation: cancellation.clone(),
                }))
                .await;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let linked = linked.map_err(|message| ProfileError::Provider {
                provider: provider_id,
                message,
            })?;
            let provider = provider.metadata();
            let _write = cancellation
                .run_until_cancelled(profiles.inner.write_lock.lock())
                .await
                .ok_or(Error::Cancelled)?;
            let index = validate_account_link(&profiles.state(), profile_id, &provider.id)?;
            let LinkedAccount {
                identity,
                credential,
            } = linked;
            let account = AccountLink::new(provider, identity);
            if let Some(secret) = credential.as_deref() {
                credentials::save(account.link_id, secret).await?;
            }
            let result = profiles
                .update_locked(|state| {
                    state.profiles[index].accounts.push(account.clone());
                    Ok(account.clone())
                })
                .await;
            if let Err(error) = result {
                if credential.is_some()
                    && let Err(cleanup) = credentials::delete(account.link_id).await
                {
                    return Err(ProfileError::AccountLinkRollback {
                        link_id: account.link_id,
                        source: Box::new(error),
                        cleanup,
                    }
                    .into());
                }
                return Err(error);
            }
            result
        })
    }

    /// Removes an account link and deletes its stored credential.
    ///
    /// Link membership is persisted before credential deletion. An unknown
    /// `link_id` therefore still attempts credential deletion, making the
    /// method suitable for retrying cleanup after a partial failure.
    ///
    /// # Errors
    ///
    /// Returns a persistence error if the changed profile state cannot be
    /// saved, or [`ProfileError::CredentialCleanup`] if the credential store
    /// cannot delete the secret. A missing credential is treated as success.
    pub async fn unlink_account(&self, link_id: Uuid) -> Result<()> {
        let _write = self.inner.write_lock.lock().await;
        self.update_locked(|state| {
            for profile in &mut state.profiles {
                profile
                    .accounts
                    .retain(|account| account.link_id != link_id);
            }
            Ok(())
        })
        .await?;
        credentials::delete(link_id)
            .await
            .map_err(|source| ProfileError::CredentialCleanup { link_id, source })?;
        Ok(())
    }
}

fn validate_account_link(
    state: &ProfilesState,
    profile_id: Uuid,
    provider_id: &str,
) -> Result<usize> {
    let profile_index = state
        .profiles
        .iter()
        .position(|profile| profile.id == profile_id)
        .ok_or(ProfileError::NotFound(profile_id))?;
    if state.profiles[profile_index]
        .accounts
        .iter()
        .any(|account| account.provider.id == provider_id)
    {
        return Err(ProfileError::AccountAlreadyLinked {
            profile: profile_id,
            provider: provider_id.to_owned(),
        }
        .into());
    }
    Ok(profile_index)
}
