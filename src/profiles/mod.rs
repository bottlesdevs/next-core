//! Persisted application profiles and selection.

mod accounts;
mod credentials;
mod error;
mod providers;
mod state;

pub use error::ProfileError;
pub use providers::{AccountIdentity, AccountLinkInteraction, AccountProviderInfo};
pub use state::{AccountLink, Profile, ProfilesState};

use crate::{Directories, error::Result};
use bottles_plugin_host::Plugins;
use futures_core::Stream;
use std::{io, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

struct ProfilesInner {
    plugins: Arc<Plugins>,
    path: PathBuf,
    published: watch::Sender<Arc<ProfilesState>>,
    write_lock: Mutex<()>,
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
        operation: impl FnOnce(&mut ProfilesState) -> Result<T>,
    ) -> Result<T> {
        let _write = self.inner.write_lock.lock().await;
        self.update_locked(operation).await
    }

    /// Caller holds write_lock through membership changes and credential cleanup.
    async fn update_locked<T>(
        &self,
        operation: impl FnOnce(&mut ProfilesState) -> Result<T>,
    ) -> Result<T> {
        let current = self.inner.published.borrow().clone();
        let mut next = current.as_ref().clone();
        let value = operation(&mut next)?;
        if next == *current {
            return Ok(value);
        }
        next_config::save(&self.inner.path, &next).await?;
        self.inner.published.send_replace(Arc::new(next));
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
                let state = ProfilesState::player();
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
            plugins,
            path,
            published,
            write_lock: Mutex::new(()),
        });
        Ok(Self { inner })
    }

    /// Returns the current profile collection and selection atomically.
    pub fn state(&self) -> Arc<ProfilesState> {
        self.inner.published.borrow().clone()
    }

    /// Returns every profile in persisted order.
    pub fn list(&self) -> Vec<Profile> {
        self.state().profiles().to_vec()
    }

    /// Returns the selected profile.
    pub fn selected(&self) -> Profile {
        self.state().selected().clone()
    }

    /// Watches coherent profile collection and selection snapshots.
    ///
    /// The stream yields the current snapshot first. Slow consumers may miss
    /// intermediate changes and receive only the latest coherent snapshot.
    pub fn watch(&self) -> impl Stream<Item = Arc<ProfilesState>> + Send + 'static + use<> {
        WatchStream::new(self.inner.published.subscribe())
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
            let state = self.state();
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
}
