//! Persisted application profiles and selection.

use std::{
    collections::HashMap,
    io,
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

use crate::{Context, error::Result};

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
    context: Context,
    published: watch::Sender<Arc<ProfilesConfig>>,
    write_lock: Mutex<()>,
    providers: RwLock<HashMap<NonNilUuid, Arc<dyn StorefrontAccountProvider>>>,
}

/// The persisted collection of application profiles.
#[derive(Clone)]
pub struct Profiles(Arc<ProfilesInner>);

impl Profiles {
    pub(crate) async fn load(context: Context) -> Result<Self> {
        let path = context.directories().profiles();
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
        Ok(Self::new(context, state))
    }

    fn new(context: Context, state: ProfilesConfig) -> Self {
        let (published, _) = watch::channel(Arc::new(state));
        Self(Arc::new(ProfilesInner {
            context,
            published,
            write_lock: Mutex::new(()),
            providers: RwLock::new(HashMap::new()),
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
        self.update(move |state| {
            if state.selected == id {
                return Err(ProfileError::Selected(id).into());
            }
            let index = state
                .profiles
                .iter()
                .position(|profile| profile.id == id)
                .ok_or(ProfileError::NotFound(id))?;
            state.profiles.remove(index);
            Ok(())
        })
        .await
    }

    /// Returns the storefront providers available in this process.
    pub fn account_providers(&self) -> Vec<StorefrontProvider> {
        self.0
            .providers
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|provider| provider.provider())
            .collect()
    }

    /// Registers or replaces the provider with the same stable identity.
    pub fn register_account_provider(&self, provider: Arc<dyn StorefrontAccountProvider>) {
        let id = provider.provider().id;
        self.0
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id, provider);
    }

    /// Removes an available provider without changing persisted accounts.
    pub fn unregister_account_provider(&self, provider: NonNilUuid) {
        self.0
            .providers
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&provider);
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
            let profile = state
                .profiles
                .iter_mut()
                .find(|profile| profile.id == profile_id)
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
        self.update(move |state| {
            let profile = state
                .profiles
                .iter_mut()
                .find(|profile| profile.id == profile_id)
                .ok_or(ProfileError::NotFound(profile_id))?;
            let account = profile
                .accounts
                .iter()
                .position(|account| account.provider.id == provider_id)
                .ok_or(ProfileError::AccountNotLinked {
                    profile: profile_id,
                    provider: provider_id,
                })?;
            profile.accounts.remove(account);
            Ok(profile.clone())
        })
        .await
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
        next_config::save(&self.0.context.directories().profiles(), &next).await?;
        self.0.published.send_replace(Arc::new(next));
        Ok(value)
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
