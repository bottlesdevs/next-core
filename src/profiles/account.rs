use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

use super::{AccountIdentity, ProfileError, StorefrontProvider, storefront};
use crate::{Plugins, credentials, error::Result};

/// A linked storefront account. Clones share credential operations and removal state.
/// Only provider and identity metadata are persisted or compared for equality.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StorefrontAccount {
    pub provider: StorefrontProvider,
    pub identity: AccountIdentity,
    // True once cleanup starts; the same mutex serializes credential operations.
    #[serde(skip)]
    operation: Arc<Mutex<bool>>,
}

impl PartialEq for StorefrontAccount {
    fn eq(&self, other: &Self) -> bool {
        self.provider == other.provider && self.identity == other.identity
    }
}

impl Eq for StorefrontAccount {}

impl StorefrontAccount {
    pub(super) fn new(
        provider: StorefrontProvider,
        identity: bottles_plugin_host::AccountIdentity,
    ) -> Self {
        Self {
            provider,
            identity: AccountIdentity {
                account_id: identity.account_id,
                display_name: identity.display_name,
            },
            operation: Arc::default(),
        }
    }

    pub(super) fn same_link(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.operation, &other.operation)
    }

    pub(super) async fn lock(&self) -> OwnedMutexGuard<bool> {
        self.operation.clone().lock_owned().await
    }

    pub(super) async fn prepare(
        &self,
        profile_id: Uuid,
        credential: Option<&[u8]>,
        guard: Arc<OwnedMutexGuard<()>>,
    ) -> Result<()> {
        match credential {
            Some(secret) => credentials::save(&self.provider.id, profile_id, secret, guard).await?,
            None => credentials::delete(&self.provider.id, profile_id, guard).await?,
        }
        Ok(())
    }

    /// For a published account, caller retains its operation lock through removal.
    /// The supplied profile write guard also excludes new links during cleanup.
    /// Failed cleanup leaves the account unavailable for searches and can be retried.
    pub(super) async fn cleanup(
        &self,
        profile_id: Uuid,
        operation: &mut OwnedMutexGuard<bool>,
        guard: Arc<OwnedMutexGuard<()>>,
    ) -> Result<()> {
        **operation = true;
        credentials::delete(&self.provider.id, profile_id, guard).await?;
        Ok(())
    }

    /// Serialize credential load, provider execution, and refresh persistence.
    /// Capability resolution stays lazy; absent credentials are passed through.
    /// Account-only plugins return None, while unavailable providers return errors.
    pub(super) async fn owned_games(
        &self,
        plugins: &Plugins,
        profile_id: Uuid,
    ) -> Result<Option<(String, Vec<bottles_plugin_host::OwnedGame>)>> {
        let guard = self.lock().await;
        if *guard {
            return Err(ProfileError::AccountNotLinked {
                profile: profile_id,
                provider: self.provider.id.clone(),
            }
            .into());
        }
        let guard = Arc::new(guard);
        let Some(provider) = storefront::library_provider(plugins, &self.provider.id)? else {
            return Ok(None);
        };
        let credential = credentials::load(&self.provider.id, profile_id).await?;
        let listed = provider
            .list_games(&self.identity.account_id, credential.as_deref())
            .await
            .map_err(|message| ProfileError::Provider {
                provider: self.provider.id.clone(),
                message,
            })?;
        // Persist refreshes even when the subsequent enumeration failed.
        if let Some(updated) = listed.updated_credential.as_deref() {
            if let Err(error) =
                credentials::save(&self.provider.id, profile_id, updated, guard.clone()).await
            {
                tracing::warn!(provider = %self.provider.id, profile = %profile_id,
                    "failed to save refreshed storefront credential: {error}");
            }
        }
        let games = listed.games.map_err(|message| ProfileError::Provider {
            provider: self.provider.id.clone(),
            message,
        })?;
        Ok(Some((provider.name().to_owned(), games)))
    }
}
