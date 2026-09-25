//! Account-provider discovery and profile-link lifecycle management.
//!
//! Account metadata is persisted with the profile, while provider credentials
//! are kept in the platform credential store and keyed by link UUID.

use super::providers::LinkedAccount;
use super::{
    AccountLink, AccountLinkInteraction, ProfileError, Profiles, ProfilesState, credentials,
};
use crate::{
    Operation,
    error::{Error, Result},
};
use std::sync::Arc;
use uuid::Uuid;

impl Profiles {
    /// Starts linking an account provider to a profile.
    ///
    /// The returned [`Operation`] must be driven by the caller. It validates
    /// the profile and provider membership, asks the provider to identify or
    /// authenticate an account, stores any returned credential, and finally
    /// publishes the new [`AccountLink`]. A profile can have at most one link
    /// for each provider.
    ///
    /// Cancellation drops the pending provider call, including its interaction.
    /// An interrupted plugin invocation closes that provider's session.
    /// After the write lock is acquired,
    /// cancellation is no longer checked; final validation, credential storage,
    /// snapshot persistence, and any rollback run to completion.
    ///
    /// # Errors
    ///
    /// When driven, the operation returns [`ProfileError::NotFound`] for an
    /// unknown profile, [`ProfileError::AccountAlreadyLinked`] for a duplicate
    /// provider, or [`ProfileError::Provider`] when the provider rejects the
    /// request or is not registered. Credential storage, persistence, cancellation,
    /// and rollback failures are also returned. Cancellation is reported as
    /// [`Error::Cancelled`] only before the write-lock boundary described above.
    pub fn link_account(
        &self,
        profile_id: Uuid,
        provider_id: String,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Operation<AccountLink> {
        let profiles = self.clone();
        Operation::new(move |_, cancellation| async move {
            validate_account_link(&profiles.state(), profile_id, &provider_id)?;
            let provider = profiles
                .inner
                .providers
                .read()
                .unwrap()
                .get(&provider_id)
                .cloned()
                .ok_or_else(|| ProfileError::Provider {
                    provider: provider_id.clone(),
                    message: "account provider is not registered".into(),
                })?;
            let linked = cancellation
                .run_until_cancelled(provider.link_account(interaction))
                .await
                .ok_or(Error::Cancelled)?;
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
    /// When present, link removal is persisted and published before credential
    /// deletion. An unknown `link_id` skips persistence and publication but still
    /// attempts credential deletion, making the method suitable for retrying cleanup
    /// after a partial failure.
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
