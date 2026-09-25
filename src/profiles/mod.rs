//! Persistent user profiles, account links, and active-profile selection.
//!
//! [`Profiles`] owns the live profile collection. Mutations are serialized,
//! saved to disk before publication, and exposed to observers as coherent
//! [`ProfilesState`] snapshots.
//! Linked-account metadata is part of those snapshots; provider secrets are
//! stored separately in the platform credential store.

mod accounts;
mod credentials;
mod error;
mod providers;
mod state;

pub use error::ProfileError;
pub use providers::{
    AccountIdentity, AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount,
};
pub use state::{AccountLink, Profile, ProfilesState};

use crate::{Directories, error::Result};
use futures_core::Stream;
use std::{
    collections::HashMap,
    io,
    path::PathBuf,
    sync::{Arc, RwLock},
};
use tokio::sync::{Mutex, watch};
use tokio_stream::wrappers::WatchStream;
use uuid::Uuid;

struct ProfilesInner {
    providers: RwLock<HashMap<String, Arc<dyn AccountProvider>>>,
    path: PathBuf,
    published: watch::Sender<Arc<ProfilesState>>,
    write_lock: Mutex<()>,
}

impl Profiles {
    /// Makes the profile identified by `id` the selected profile.
    ///
    /// The updated selection is persisted before it is published to watchers.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::NotFound`] if `id` does not identify a profile,
    /// or a persistence error if the updated state cannot be saved.
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

    /// Applies, persists, and publishes a mutation while the caller holds `write_lock`.
    ///
    /// If the mutation leaves the snapshot unchanged, persistence and publication
    /// are skipped.
    ///
    /// # Errors
    ///
    /// Returns an error from `operation` or from persisting the changed snapshot.
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

/// Manages the persisted collection of application profiles.
///
/// Clones share the same state, write lock, and change notifications. Mutating
/// methods persist a complete snapshot before making it visible through
/// [`state`](Self::state) or [`watch`](Self::watch). A profile-state update that
/// leaves the snapshot unchanged is neither saved nor published.
///
/// # Examples
///
/// ```
/// # fn inspect(core: &bottles_core::Bottles) {
/// let profiles = core.profiles();
/// let selected = profiles.selected();
/// assert_eq!(selected.id(), profiles.state().selected().id());
/// # }
/// ```
#[derive(Clone)]
pub struct Profiles {
    inner: Arc<ProfilesInner>,
}

impl Profiles {
    /// Loads persisted profiles or creates the initial `Player` profile.
    ///
    /// # Errors
    ///
    /// Returns an error if state cannot be loaded or initialized, or
    /// [`ProfileError::NotFound`] if the persisted selection is invalid.
    pub(crate) async fn load(directories: &Directories) -> Result<Self> {
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
            providers: RwLock::new(providers::builtins()),
            path,
            published,
            write_lock: Mutex::new(()),
        });
        Ok(Self { inner })
    }

    /// Returns the current profile collection and selection in one snapshot.
    ///
    /// The returned [`Arc`] remains unchanged when later mutations are
    /// published.
    pub fn state(&self) -> Arc<ProfilesState> {
        self.inner.published.borrow().clone()
    }

    /// Returns a copy of every profile in persisted order.
    pub fn list(&self) -> Vec<Profile> {
        self.state().profiles().to_vec()
    }

    /// Returns a copy of the currently selected profile.
    pub fn selected(&self) -> Profile {
        self.state().selected().clone()
    }

    /// Streams coherent profile collection and selection snapshots.
    ///
    /// The stream yields the current snapshot first. Slow consumers may miss
    /// intermediate changes and receive only the latest coherent snapshot.
    pub fn watch(&self) -> impl Stream<Item = Arc<ProfilesState>> + Send + 'static + use<> {
        WatchStream::new(self.inner.published.subscribe())
    }

    /// Creates a profile and selects it in the same published update.
    ///
    /// Leading and trailing whitespace is removed from `name`; no other name
    /// validation is performed. The profile is assigned a new UUID, initially has
    /// no linked accounts, and becomes selected in the same published snapshot.
    ///
    /// # Errors
    ///
    /// Returns a persistence error if the updated profile state cannot be saved.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{Profiles, error::Error};
    /// # async fn example(profiles: &Profiles) -> Result<(), Error> {
    /// let profile = profiles.create("Second player").await?;
    /// assert_eq!(profile.name(), "Second player");
    /// assert_eq!(profiles.selected().id(), profile.id());
    /// # Ok(())
    /// # }
    /// ```
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

    /// Changes the display name of the profile identified by `id`.
    ///
    /// Leading and trailing whitespace is removed from `name`; no other name
    /// validation is performed.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::NotFound`] if `id` does not identify a profile,
    /// or a persistence error if the updated state cannot be saved.
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

    /// Deletes the profile identified by `id`.
    ///
    /// Linked accounts are removed one at a time in stored order before the profile
    /// itself is removed. If `id` is selected, the first remaining profile becomes
    /// selected. At least one profile is always retained. Each successful account
    /// unlink is persisted immediately, so a later credential-cleanup or persistence
    /// failure can leave the profile present with earlier links already removed.
    ///
    /// # Errors
    ///
    /// Returns [`ProfileError::NotFound`] when `id` is unknown,
    /// [`ProfileError::LastProfile`] when it is the sole remaining profile, or
    /// [`ProfileError::CredentialCleanup`] if a linked account is removed but
    /// its stored credential cannot be deleted. Persistence failures are also
    /// returned.
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
