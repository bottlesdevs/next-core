//! Persisted application profiles and selection.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::{NonNilUuid, Uuid};

use crate::{
    Directories,
    credentials::CredentialStore,
    error::{Error, Result},
};

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

/// Static identity of one available storefront account provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: NonNilUuid,
    pub name: String,
}

/// Public account metadata returned by a storefront provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    pub account_id: String,
    pub display_name: String,
}

/// Native extension point for linking one storefront account.
#[async_trait]
pub trait StorefrontAccountProvider: Send + Sync {
    fn provider(&self) -> StorefrontProvider;

    async fn link_account(&self, profile_id: Uuid) -> std::result::Result<AccountIdentity, String>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
struct ProfilesConfig {
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
}

struct ProfilesInner {
    path: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    published: watch::Sender<Arc<ProfilesConfig>>,
    write_lock: Mutex<()>,
    providers: RwLock<AccountProviders>,
}

#[derive(Default)]
struct AccountProviders {
    values: HashMap<NonNilUuid, Arc<dyn StorefrontAccountProvider>>,
    built_in: HashSet<NonNilUuid>,
}

/// The persisted collection of application profiles.
#[derive(Clone)]
pub struct Profiles(Arc<ProfilesInner>);

impl Profiles {
    pub(crate) async fn load(
        directories: &Directories,
        credentials: Arc<dyn CredentialStore>,
    ) -> Result<Self> {
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
        Ok(Self::new(path, credentials, state))
    }

    fn new(path: PathBuf, credentials: Arc<dyn CredentialStore>, state: ProfilesConfig) -> Self {
        let (published, _) = watch::channel(Arc::new(state));
        Self(Arc::new(ProfilesInner {
            path,
            credentials,
            published,
            write_lock: Mutex::new(()),
            providers: RwLock::new(AccountProviders::default()),
        }))
    }

    /// Returns every profile in unspecified order.
    pub fn list(&self) -> Vec<Profile> {
        self.0.published.borrow().profiles.clone()
    }

    /// Returns the selected profile.
    pub fn selected(&self) -> Profile {
        let state = self.0.published.borrow();
        state
            .profile(state.selected)
            .cloned()
            .expect("selected profile was validated")
    }

    /// Watches the complete profile collection.
    ///
    /// The stream yields the current collection first. Slow consumers may miss
    /// intermediate changes and receive only the latest snapshot. Ordering is
    /// unspecified. Selection changes also emit; use [`Profiles::selected`] to
    /// read the current selection.
    pub fn watch(&self) -> impl Stream<Item = Vec<Profile>> + Send + 'static {
        WatchStream::new(self.0.published.subscribe()).map(|state| state.profiles.clone())
    }

    /// Watches the selected profile, yielding its current snapshot first.
    pub fn watch_selected(&self) -> impl Stream<Item = Profile> + Send + 'static {
        let mut previous = None;
        WatchStream::new(self.0.published.subscribe()).filter_map(move |state| {
            let selected = state
                .profile(state.selected)
                .cloned()
                .expect("selected profile was validated");
            if previous.as_ref() == Some(&selected) {
                None
            } else {
                previous = Some(selected.clone());
                Some(selected)
            }
        })
    }

    /// Creates an unselected profile with a generated UUID.
    pub async fn create(&self, name: impl Into<String>) -> Result<Profile> {
        let name = profile_name(name)?;
        self.update(move |state| {
            let profile = Profile {
                id: Uuid::new_v4(),
                name,
                accounts: Vec::new(),
            };
            state.profiles.push(profile.clone());
            Ok(profile)
        })
        .await
    }

    /// Renames an existing profile.
    pub async fn rename(&self, id: Uuid, name: impl Into<String>) -> Result<Profile> {
        let name = profile_name(name)?;
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

    /// Deletes an existing unselected profile.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let _write = self.0.write_lock.lock().await;
        let current = self.0.published.borrow().clone();
        if current.selected == id {
            return Err(ProfileError::Selected(id).into());
        }
        let index = current
            .profiles
            .iter()
            .position(|profile| profile.id == id)
            .ok_or(ProfileError::NotFound(id))?;
        for account in &current.profiles[index].accounts {
            self.0
                .credentials
                .delete(account.provider.id, id)
                .await
                .map_err(|error| Error::Credential(error.to_string()))?;
        }
        let mut next = current.as_ref().clone();
        next.profiles.remove(index);
        self.persist(next).await
    }

    /// Returns the storefront providers available in this process.
    pub fn account_providers(&self) -> Vec<StorefrontProvider> {
        self.0
            .providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values
            .values()
            .map(|provider| provider.provider())
            .collect()
    }

    /// Registers or replaces an extension-provided account provider.
    pub fn register_account_provider(
        &self,
        provider: Arc<dyn StorefrontAccountProvider>,
    ) -> Result<()> {
        let id = provider.provider().id;
        let mut providers = self
            .0
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if providers.built_in.contains(&id) {
            return Err(ProfileError::ProviderBuiltIn(id).into());
        }
        providers.values.insert(id, provider);
        Ok(())
    }

    /// Removes an available provider without changing persisted accounts.
    pub fn unregister_account_provider(&self, provider: NonNilUuid) -> Result<()> {
        let mut providers = self
            .0
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if providers.built_in.contains(&provider) {
            return Err(ProfileError::ProviderBuiltIn(provider).into());
        }
        providers.values.remove(&provider);
        Ok(())
    }

    pub(crate) fn register_builtin_account_provider(
        &self,
        provider: Arc<dyn StorefrontAccountProvider>,
    ) {
        let id = provider.provider().id;
        let mut providers = self
            .0
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        providers.values.insert(id, provider);
        providers.built_in.insert(id);
    }

    /// Links one account through an available provider and persists its public metadata.
    pub async fn link_account(&self, profile_id: Uuid, provider_id: NonNilUuid) -> Result<Profile> {
        let profile = self
            .0
            .published
            .borrow()
            .profile(profile_id)
            .cloned()
            .ok_or(ProfileError::NotFound(profile_id))?;
        if profile
            .accounts
            .iter()
            .any(|account| account.provider.id == provider_id)
        {
            return Err(ProfileError::AccountAlreadyLinked {
                profile: profile_id,
                provider: provider_id,
            }
            .into());
        }
        let provider = self
            .0
            .providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values
            .get(&provider_id)
            .cloned()
            .ok_or(ProfileError::ProviderNotFound(provider_id))?;
        let provider_info = provider.provider();
        let identity =
            provider
                .link_account(profile_id)
                .await
                .map_err(|error| ProfileError::Provider {
                    provider: provider_id,
                    message: error,
                })?;

        self.update(move |state| {
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
                    provider: provider_id,
                }
                .into());
            }
            if let Some(owner) = state.profiles.iter().find(|profile| {
                profile.accounts.iter().any(|account| {
                    account.provider.id == provider_id
                        && account.identity.account_id == identity.account_id
                })
            }) {
                return Err(ProfileError::AccountIdentityAlreadyLinked {
                    profile: owner.id,
                    provider: provider_id,
                    account_id: identity.account_id.clone(),
                }
                .into());
            }
            let profile = &mut state.profiles[profile_index];
            profile.accounts.push(StorefrontAccount {
                provider: provider_info,
                identity,
            });
            Ok(profile.clone())
        })
        .await
    }

    /// Removes persisted account metadata without requiring its provider.
    pub async fn unlink_account(
        &self,
        profile_id: Uuid,
        provider_id: NonNilUuid,
    ) -> Result<Profile> {
        let _write = self.0.write_lock.lock().await;
        let current = self.0.published.borrow().clone();
        let profile_index = current
            .profiles
            .iter()
            .position(|profile| profile.id == profile_id)
            .ok_or(ProfileError::NotFound(profile_id))?;
        let account_index = current.profiles[profile_index]
            .accounts
            .iter()
            .position(|account| account.provider.id == provider_id)
            .ok_or(ProfileError::AccountNotLinked {
                profile: profile_id,
                provider: provider_id,
            })?;
        self.0
            .credentials
            .delete(provider_id, profile_id)
            .await
            .map_err(|error| Error::Credential(error.to_string()))?;
        let mut next = current.as_ref().clone();
        next.profiles[profile_index].accounts.remove(account_index);
        let profile = next.profiles[profile_index].clone();
        self.persist(next).await?;
        Ok(profile)
    }

    pub(crate) async fn select_account(
        &self,
        provider_id: NonNilUuid,
        account_id: &str,
    ) -> Result<Option<Profile>> {
        let profile_id = self
            .0
            .published
            .borrow()
            .profiles
            .iter()
            .find(|profile| {
                profile.accounts.iter().any(|account| {
                    account.provider.id == provider_id && account.identity.account_id == account_id
                })
            })
            .map(|profile| profile.id);
        match profile_id {
            Some(profile_id) => self.select(profile_id).await.map(Some),
            None => Ok(None),
        }
    }

    async fn update<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesConfig) -> Result<T>,
    ) -> Result<T> {
        let _write = self.0.write_lock.lock().await;
        let current = self.0.published.borrow().clone();
        let mut next = current.as_ref().clone();
        let value = operation(&mut next)?;
        if next == *current {
            return Ok(value);
        }
        self.persist(next).await?;
        Ok(value)
    }

    async fn persist(&self, next: ProfilesConfig) -> Result<()> {
        next_config::save(&self.0.path, &next).await?;
        self.0.published.send_replace(Arc::new(next));
        Ok(())
    }
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
