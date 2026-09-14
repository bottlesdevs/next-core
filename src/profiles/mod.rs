//! Persisted application profiles and selection.

mod error;
mod plugin;
mod steam;

pub use error::ProfileError;
use steam::SteamIntegration;

use std::{borrow::Cow, io, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{Directories, Operation, PluginId, PluginKind, Plugins, credentials, error::Result};

/// Static identity of one available storefront account provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: PluginId,
    pub name: Cow<'static, str>,
}

/// Public account metadata returned by a storefront provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    pub account_id: String,
    pub display_name: String,
}

/// Account metadata and credential produced by a successful provider link.
struct LinkedAccount {
    pub identity: AccountIdentity,
    pub credential: Option<Vec<u8>>,
}

/// Supplies provider-directed interaction while an account is being linked.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: url::Url,
        instructions: String,
    ) -> std::result::Result<String, String>;
}

/// Links one storefront account through a native or Wasm provider.
#[async_trait]
trait StorefrontAccountProvider: Send + Sync {
    fn metadata(&self) -> StorefrontProvider;

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> std::result::Result<LinkedAccount, String>;
}

/// One coherent persisted snapshot of every profile and the selected profile.
///
/// The selected profile is guaranteed to be present in [`profiles`](Self::profiles).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct ProfilesConfig {
    selected: Uuid,
    profiles: Vec<Profile>,
}

impl ProfilesConfig {
    fn player() -> Self {
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "Player".into(),
            accounts: Vec::new(),
        };
        Self {
            selected: profile.id,
            profiles: vec![profile],
        }
    }

    fn profile(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Returns every profile in persisted order.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Returns the selected profile from this same snapshot generation.
    pub fn selected(&self) -> &Profile {
        self.profile(self.selected)
            .expect("selected profile was validated")
    }
}

struct ProfilesInner {
    path: PathBuf,
    published: watch::Sender<Arc<ProfilesConfig>>,
    write_lock: Mutex<()>,
}

impl ProfilesInner {
    async fn select(&self, id: Uuid) -> Result<Profile> {
        self.update(move |state| {
            let profile = state
                .profile(id)
                .cloned()
                .ok_or(ProfileError::NotFound(id))?;
            state.selected = id;
            Ok(profile)
        })
        .await
    }

    async fn select_account(
        &self,
        provider_id: &PluginId,
        account_id: &str,
    ) -> Result<Option<Profile>> {
        self.update(|state| {
            let Some(profile) = state.profiles.iter().find(|profile| {
                profile.accounts.iter().any(|account| {
                    &account.provider.id == provider_id && account.identity.account_id == account_id
                })
            }) else {
                return Ok(None);
            };
            let profile = profile.clone();
            state.selected = profile.id;
            Ok(Some(profile))
        })
        .await
    }

    async fn update<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesConfig) -> Result<T>,
    ) -> Result<T> {
        let _write = self.write_lock.lock().await;
        self.update_locked(operation).await
    }

    /// Caller holds write_lock, including any credential cleanup after publication.
    async fn update_locked<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesConfig) -> Result<T>,
    ) -> Result<T> {
        let current = self.published.borrow().clone();
        let mut next = current.as_ref().clone();
        let value = operation(&mut next)?;
        if next == *current {
            return Ok(value);
        }
        self.persist(next).await?;
        Ok(value)
    }

    async fn persist(&self, next: ProfilesConfig) -> Result<()> {
        next_config::save(&self.path, &next).await?;
        self.published.send_replace(Arc::new(next));
        Ok(())
    }
}

/// The persisted collection of application profiles.
///
/// Clones share one live collection.
#[derive(Clone)]
pub struct Profiles {
    steam: Arc<SteamIntegration>,
    plugins: Arc<Plugins>,
    inner: Arc<ProfilesInner>,
}

impl Profiles {
    pub(crate) async fn load(directories: &Directories, plugins: Arc<Plugins>) -> Result<Self> {
        let path = directories.profiles();
        let state = match next_config::load(&path).await {
            Ok(state) => state,
            Err(next_config::error::Error::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                let state = ProfilesConfig::player();
                next_config::save(&path, &state).await?;
                state
            }
            Err(error) => return Err(error.into()),
        };
        if state.profile(state.selected).is_none() {
            return Err(ProfileError::NotFound(state.selected).into());
        }
        let (published, _) = watch::channel(Arc::new(state));
        let inner = Arc::new(ProfilesInner {
            path,
            published,
            write_lock: Mutex::new(()),
        });
        let steam = Arc::new(SteamIntegration::open(inner.clone()).await);
        Ok(Self {
            steam,
            plugins,
            inner,
        })
    }

    /// Returns the current profile collection and selection atomically.
    pub fn snapshot(&self) -> Arc<ProfilesConfig> {
        self.inner.published.borrow().clone()
    }

    /// Returns every profile in persisted order.
    pub fn list(&self) -> Vec<Profile> {
        self.snapshot().profiles().to_vec()
    }

    /// Returns the selected profile.
    pub fn selected(&self) -> Profile {
        self.snapshot().selected().clone()
    }

    /// Watches coherent profile collection and selection snapshots.
    ///
    /// The stream yields the current snapshot first. Slow consumers may miss
    /// intermediate changes and receive only the latest coherent snapshot.
    pub fn watch(&self) -> impl Stream<Item = Arc<ProfilesConfig>> + Send + 'static + use<> {
        WatchStream::new(self.inner.published.subscribe())
    }

    /// Creates and selects a profile with a generated UUID in one publication.
    pub async fn create(&self, name: impl Into<String>) -> Result<Profile> {
        let name = profile_name(name)?;
        self.inner
            .update(move |state| {
                let profile = Profile {
                    id: Uuid::new_v4(),
                    name,
                    accounts: Vec::new(),
                };
                state.profiles.push(profile.clone());
                state.selected = profile.id;
                Ok(profile)
            })
            .await
    }

    /// Renames an existing profile.
    pub async fn rename(&self, id: Uuid, name: impl Into<String>) -> Result<Profile> {
        let name = profile_name(name)?;
        self.inner
            .update(move |state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == id)
                    .ok_or(ProfileError::NotFound(id))?;
                profile.name = name;
                Ok(profile.clone())
            })
            .await
    }

    /// Selects an existing profile.
    pub async fn select(&self, id: Uuid) -> Result<Profile> {
        self.inner.select(id).await
    }

    /// Deletes an existing profile.
    ///
    /// Deleting the selected profile selects the first remaining profile in the
    /// same persisted update. The only remaining profile cannot be deleted.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let _write = self.inner.write_lock.lock().await;
        let profile = self
            .inner
            .update_locked(|state| {
                let index = state
                    .profiles
                    .iter()
                    .position(|profile| profile.id == id)
                    .ok_or(ProfileError::NotFound(id))?;
                if state.profiles.len() == 1 {
                    return Err(ProfileError::LastProfile(id).into());
                }
                let removed = state.profiles.remove(index);
                if state.selected == id {
                    state.selected = state.profiles[0].id;
                }
                Ok(removed)
            })
            .await?;
        for account in profile.accounts {
            if let Err(error) = credentials::delete(&account.provider.id, id).await {
                tracing::warn!(
                    provider = %account.provider.id,
                    profile = %id,
                    "failed to delete credential after profile deletion: {error}"
                );
            }
        }
        Ok(())
    }

    /// Read games for the binding captured by a search, without exposing credentials.
    /// Provider calls run outside the profile write lock so storefronts remain concurrent.
    pub(crate) async fn owned_games(
        &self,
        profile_id: Uuid,
        account: &StorefrontAccount,
    ) -> Result<(String, Vec<bottles_plugin_host::OwnedGame>)> {
        let provider_id = &account.provider.id;
        let plugin = self
            .plugins
            .contribution(provider_id, PluginKind::StorefrontLibraryProvider)
            .ok_or_else(|| ProfileError::ProviderNotFound(provider_id.clone()))?;
        let credential = {
            let _write = self.inner.write_lock.lock().await;
            self.require_binding(profile_id, account)?;
            credentials::load(provider_id, profile_id)
                .await?
                .ok_or_else(|| ProfileError::Provider {
                    provider: provider_id.clone(),
                    message: "storefront credential is missing".into(),
                })?
        };
        let listed = plugin
            .runtime
            .list_games(&account.identity.account_id, Some(&credential))
            .await
            .map_err(|message| ProfileError::Provider {
                provider: provider_id.clone(),
                message,
            })?;
        if let Some(updated) = listed.updated_credential.as_deref() {
            let _write = self.inner.write_lock.lock().await;
            // An in-flight request must not recreate an unlinked account's credential.
            if self.require_binding(profile_id, account).is_ok() {
                if let Err(error) = credentials::save(provider_id, profile_id, updated).await {
                    tracing::warn!(provider = %provider_id, profile = %profile_id,
                        "failed to save refreshed storefront credential: {error}");
                }
            }
        }
        Ok((plugin.manifest.name.clone(), listed.games))
    }

    fn require_binding(&self, profile_id: Uuid, account: &StorefrontAccount) -> Result<()> {
        let state = self.snapshot();
        let profile = state
            .profile(profile_id)
            .ok_or(ProfileError::NotFound(profile_id))?;
        if profile.accounts.iter().any(|linked| {
            linked.provider.id == account.provider.id
                && linked.identity.account_id == account.identity.account_id
        }) {
            Ok(())
        } else {
            Err(ProfileError::AccountNotLinked {
                profile: profile_id,
                provider: account.provider.id.clone(),
            }
            .into())
        }
    }

    /// Returns the storefront providers available in this process.
    pub fn account_providers(&self) -> Vec<StorefrontProvider> {
        let plugins = self
            .plugins
            .contributions(PluginKind::StorefrontAccountProvider)
            .into_iter()
            .map(|provider| provider.metadata())
            .collect();
        merge_account_providers(plugins)
    }

    /// Links one account through an available provider.
    ///
    /// Provider authentication and caller interaction happen without holding
    /// the profile write lock. Cancellation is observed during authentication
    /// and while waiting for that lock. Once the lock is acquired, the provider
    /// and profile are revalidated before a non-cancellable persistence step.
    pub fn link_account(
        &self,
        profile_id: Uuid,
        provider_id: PluginId,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Operation<Profile> {
        let profiles = self.clone();
        Operation::new(move |_progress, cancellation| async move {
            if cancellation.is_cancelled() {
                return Err(crate::error::Error::Cancelled);
            }
            if provider_id == steam::PROVIDER_ID {
                let provider = profiles.steam.clone();
                return profiles
                    .link_account_with(profile_id, provider.as_ref(), interaction, &cancellation)
                    .await;
            }

            let provider = profiles
                .plugins
                .contribution(&provider_id, PluginKind::StorefrontAccountProvider)
                .ok_or_else(|| ProfileError::ProviderNotFound(provider_id))?;
            profiles
                .link_account_with(profile_id, &provider, interaction, &cancellation)
                .await
        })
    }

    async fn link_account_with(
        &self,
        profile_id: Uuid,
        provider: &impl StorefrontAccountProvider,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> Result<Profile> {
        if cancellation.is_cancelled() {
            return Err(crate::error::Error::Cancelled);
        }
        let metadata = provider.metadata();
        let provider_id = metadata.id.clone();
        validate_account_link(
            self.inner.published.borrow().as_ref(),
            profile_id,
            &provider_id,
        )?;

        let linked = provider.link_account(interaction, cancellation).await;
        if cancellation.is_cancelled() {
            return Err(crate::error::Error::Cancelled);
        }
        let linked = linked.map_err(|error| ProfileError::Provider {
            provider: provider_id.clone(),
            message: error,
        })?;
        let LinkedAccount {
            identity,
            credential,
        } = linked;

        let _write = cancellation
            .run_until_cancelled(self.inner.write_lock.lock())
            .await
            .ok_or(crate::error::Error::Cancelled)?;
        if cancellation.is_cancelled() {
            return Err(crate::error::Error::Cancelled);
        }
        let current = self.inner.published.borrow().clone();
        let profile_index = validate_account_link(&current, profile_id, &provider_id)?;
        let metadata = if provider_id == steam::PROVIDER_ID {
            metadata
        } else {
            self.plugins
                .contribution(&provider_id, PluginKind::StorefrontAccountProvider)
                .ok_or_else(|| ProfileError::ProviderNotFound(provider_id.clone()))?
                .metadata()
        };
        if let Some(owner) = current.profiles.iter().find(|profile| {
            profile.accounts.iter().any(|account| {
                account.provider.id == provider_id
                    && account.identity.account_id == identity.account_id
            })
        }) {
            return Err(ProfileError::AccountIdentityAlreadyLinked {
                profile: owner.id,
                provider: provider_id.clone(),
                account_id: identity.account_id.clone(),
            }
            .into());
        }

        if let Some(secret) = credential.as_deref() {
            credentials::save(&provider_id, profile_id, secret).await?;
        }
        let mut next = current.as_ref().clone();
        let profile = &mut next.profiles[profile_index];
        profile.accounts.push(StorefrontAccount {
            provider: metadata,
            identity,
        });
        let profile = profile.clone();
        self.inner.persist(next).await?;
        Ok(profile)
    }

    /// Removes persisted account metadata without requiring its provider.
    pub async fn unlink_account(&self, profile_id: Uuid, provider_id: PluginId) -> Result<Profile> {
        let _write = self.inner.write_lock.lock().await;
        let profile = self
            .inner
            .update_locked(|state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or(ProfileError::NotFound(profile_id))?;
                let account_index = profile
                    .accounts
                    .iter()
                    .position(|account| account.provider.id == provider_id)
                    .ok_or_else(|| ProfileError::AccountNotLinked {
                        profile: profile_id,
                        provider: provider_id.clone(),
                    })?;
                profile.accounts.remove(account_index);
                Ok(profile.clone())
            })
            .await?;
        if let Err(error) = credentials::delete(&provider_id, profile_id).await {
            tracing::warn!(
                provider = %provider_id,
                profile = %profile_id,
                "failed to delete credential after account unlinking: {error}"
            );
        }
        Ok(profile)
    }
}

fn validate_account_link(
    state: &ProfilesConfig,
    profile_id: Uuid,
    provider_id: &PluginId,
) -> Result<usize> {
    let profile_index = state
        .profiles
        .iter()
        .position(|profile| profile.id == profile_id)
        .ok_or(ProfileError::NotFound(profile_id))?;
    if state.profiles[profile_index]
        .accounts
        .iter()
        .any(|account| &account.provider.id == provider_id)
    {
        return Err(ProfileError::AccountAlreadyLinked {
            profile: profile_id,
            provider: provider_id.clone(),
        }
        .into());
    }
    Ok(profile_index)
}

fn merge_account_providers(mut plugins: Vec<StorefrontProvider>) -> Vec<StorefrontProvider> {
    plugins.retain(|provider| provider.id != steam::PROVIDER_ID);
    plugins.insert(0, steam::METADATA);
    plugins
}

fn profile_name(name: impl Into<String>) -> Result<String> {
    let name = name.into().trim().to_owned();
    if name.is_empty() {
        Err(ProfileError::InvalidName.into())
    } else {
        Ok(name)
    }
}

/// Public metadata for a storefront account linked to a profile.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontAccount {
    pub provider: StorefrontProvider,
    pub identity: AccountIdentity,
}

/// An immutable application-profile snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Profile {
    id: Uuid,
    name: String,
    #[serde(default)]
    accounts: Vec<StorefrontAccount>,
}

impl Profile {
    /// Returns the profile's stable identity.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the profile's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns public metadata for the storefront accounts linked to this profile.
    pub fn accounts(&self) -> &[StorefrontAccount] {
        &self.accounts
    }
}
