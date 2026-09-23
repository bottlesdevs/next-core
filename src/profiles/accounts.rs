//! Account provider discovery, linking, and credential coordination.

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
    pub fn account_providers(&self) -> Vec<AccountProviderInfo> {
        providers::list(&self.inner.plugins)
    }

    /// The caller drives linking and persistence. Cooperative cancellation resolves pending
    /// interaction and awaits accepted guest calls; entered persistence finishes before return.
    /// Dropping abandons core's continuation, but accepted guest calls may still finish.
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

    /// Remove membership before deleting the secret. An absent UUID retries cleanup.
    /// The caller must drive this future to completion once publication begins.
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
