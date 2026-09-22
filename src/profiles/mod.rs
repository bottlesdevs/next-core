//! Persisted application profiles and selection.

mod account;
mod error;
pub(crate) mod storefront;

pub use account::StorefrontAccount;
pub use error::ProfileError;

use std::{collections::HashMap, future::Future, io, path::PathBuf, sync::Arc};

use bottles_plugin_host::{Authentication, Plugins};
use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    Directories, Operation, credentials,
    error::{Error, Result},
};
use storefront::LinkedAccount;
pub use storefront::{AccountIdentity, AccountLinkInteraction, StorefrontProvider};

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

    pub(crate) fn profile(&self, id: Uuid) -> Option<&Profile> {
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
    plugins: Arc<Plugins>,
    path: PathBuf,
    published: watch::Sender<Arc<ProfilesState>>,
    write_lock: Mutex<()>,
}

// Publish the public snapshot and its credential locks as one generation.
struct ProfilesState {
    config: Arc<ProfilesConfig>,
    credential_locks: HashMap<Uuid, Arc<Mutex<()>>>,
}

impl ProfilesState {
    fn new(config: ProfilesConfig, previous: &HashMap<Uuid, Arc<Mutex<()>>>) -> Self {
        let credential_locks = config
            .profiles
            .iter()
            .flat_map(|p| &p.accounts)
            .map(|info| {
                let lock = previous
                    .get(&info.link_id)
                    .cloned()
                    .unwrap_or_else(|| Arc::new(Mutex::new(())));
                (info.link_id, lock)
            })
            .collect();
        Self {
            config: Arc::new(config),
            credential_locks,
        }
    }
}

impl Profiles {
    /// Selects an existing profile.
    pub async fn select(&self, id: Uuid) -> Result<Profile> {
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

    async fn update<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesConfig) -> Result<T>,
    ) -> Result<T> {
        let _write = self.inner.write_lock.lock().await;
        self.update_locked(operation).await
    }

    /// Caller holds write_lock through membership changes and credential cleanup.
    async fn update_locked<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesConfig) -> Result<T>,
    ) -> Result<T> {
        let current = self.inner.published.borrow().clone();
        let mut next = current.config.as_ref().clone();
        let value = operation(&mut next)?;
        if next == *current.config {
            return Ok(value);
        }
        next_config::save(&self.inner.path, &next).await?;
        self.inner
            .published
            .send_replace(Arc::new(ProfilesState::new(
                next,
                &current.credential_locks,
            )));
        Ok(value)
    }
}

/// The persisted collection of application profiles.
///
/// Clones share one live collection.
#[derive(Clone)]
pub struct Profiles {
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
        let (published, _) = watch::channel(Arc::new(ProfilesState::new(state, &HashMap::new())));
        let inner = Arc::new(ProfilesInner {
            plugins,
            path,
            published,
            write_lock: Mutex::new(()),
        });
        Ok(Self { inner })
    }

    /// Returns the current profile collection and selection atomically.
    pub fn snapshot(&self) -> Arc<ProfilesConfig> {
        self.inner.published.borrow().config.clone()
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
        WatchStream::new(self.inner.published.subscribe()).map(|state| state.config.clone())
    }

    /// Creates and selects a profile with a generated UUID in one publication.
    pub async fn create(&self, name: impl Into<String>) -> Result<Profile> {
        let name = name.into().trim().to_owned();
        self.update(move |state| {
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
        let name = name.into().trim().to_owned();
        self.update(move |state| {
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

    /// Deletes a profile after unlinking its accounts and cleaning their credentials.
    /// Failed credential cleanup leaves membership removed; retry cleanup by link UUID.
    /// Deleting the selected profile selects the first remaining profile in the
    /// final persisted update. The only remaining profile cannot be deleted.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        loop {
            let write = self.inner.write_lock.lock().await;
            let state = self.snapshot();
            let profile = state.profile(id).ok_or(ProfileError::NotFound(id))?;
            if state.profiles.len() == 1 {
                return Err(ProfileError::LastProfile(id).into());
            }
            let account = profile.accounts.first().cloned();
            let Some(account) = account else {
                return self
                    .update_locked(|state| {
                        state.profiles.retain(|profile| profile.id != id);
                        if state.selected == id {
                            state.selected = state.profiles[0].id;
                        }
                        Ok(())
                    })
                    .await;
            };
            drop(write);
            self.unlink_account(account.link_id).await?;
        }
    }

    fn account_lock(&self, link_id: Uuid) -> Option<Arc<Mutex<()>>> {
        self.inner
            .published
            .borrow()
            .credential_locks
            .get(&link_id)
            .cloned()
    }

    /// Serialize credential refresh and persistence with unlinking for this account link.
    pub(crate) async fn refresh_credential<F, Fut>(
        &self,
        profile_id: Uuid,
        link_id: Uuid,
        cancellation: &CancellationToken,
        authenticate: F,
    ) -> Result<Vec<u8>>
    where
        F: FnOnce(String, Option<Vec<u8>>) -> Fut,
        Fut: Future<Output = Result<Authentication>>,
    {
        let lock = self
            .account_lock(link_id)
            .ok_or(ProfileError::AccountNotLinked {
                profile: profile_id,
                link: link_id,
            })?;
        let _guard = cancellation
            .run_until_cancelled(lock.lock())
            .await
            .ok_or(Error::Cancelled)?;
        let snapshot = self.snapshot();
        let account = snapshot
            .profile(profile_id)
            .ok_or(ProfileError::NotFound(profile_id))?
            .accounts()
            .iter()
            .find(|account| account.link_id == link_id)
            .ok_or(ProfileError::AccountNotLinked {
                profile: profile_id,
                link: link_id,
            })?;
        // No cooperative cancellation between authentication and saving a rotated credential.
        let credential = credentials::load(link_id).await?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let auth = authenticate(account.identity.account_id.clone(), credential).await?;
        if let Some(updated) = auth.updated_credential {
            credentials::save(link_id, &updated).await?;
        }
        Ok(auth.access)
    }

    pub fn account_providers(&self) -> Vec<StorefrontProvider> {
        storefront::list(&self.inner.plugins)
    }

    /// The caller drives linking and persistence. Cooperative cancellation resolves pending
    /// interaction and awaits accepted guest calls; entered persistence finishes before return.
    /// Dropping abandons core's continuation, but accepted guest calls may still finish.
    pub fn link_account(
        &self,
        profile_id: Uuid,
        provider_id: String,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Operation<StorefrontAccount> {
        let profiles = self.clone();
        Operation::new(move |_, cancellation| async move {
            validate_account_link(&profiles.snapshot(), profile_id, &provider_id)?;
            let provider = cancellation
                .run_until_cancelled(storefront::get(&profiles.inner.plugins, &provider_id))
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
            let index = validate_account_link(&profiles.snapshot(), profile_id, &provider.id)?;
            let LinkedAccount {
                identity,
                credential,
            } = linked;
            let account = StorefrontAccount::new(provider, identity);
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

    /// Remove membership before deleting the secret. Retrying an absent UUID retries cleanup.
    /// The caller must drive this future to completion once publication begins.
    pub async fn unlink_account(&self, link_id: Uuid) -> Result<()> {
        let lock = self.account_lock(link_id);
        let _credential = match &lock {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        let write = self.inner.write_lock.lock().await;
        self.update_locked(|state| {
            for profile in &mut state.profiles {
                profile
                    .accounts
                    .retain(|account| account.link_id != link_id);
            }
            Ok(())
        })
        .await?;
        drop(write);
        credentials::delete(link_id)
            .await
            .map_err(|source| ProfileError::CredentialCleanup { link_id, source })?;
        Ok(())
    }
}

fn validate_account_link(
    state: &ProfilesConfig,
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
