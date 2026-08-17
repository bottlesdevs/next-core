//! Persisted application profiles and selection.

use std::{io, sync::Arc};

use futures_core::Stream;
use next_config::Config;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, watch};
use tokio_stream::{StreamExt, wrappers::WatchStream};
use uuid::Uuid;

use crate::{Context, error::Result};

#[derive(Debug, Error)]
pub enum ProfileError {
    /// No profile exists with the requested UUID.
    #[error("profile {0} was not found")]
    NotFound(Uuid),
    /// A profile name is empty after trimming surrounding whitespace.
    #[error("profile name must not be blank")]
    InvalidName,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
struct ProfilesConfig {
    selected: Option<Uuid>,
    profiles: Vec<Profile>,
}

impl ProfilesConfig {
    fn profile(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }
}

struct ProfilesInner {
    context: Context,
    published: watch::Sender<Arc<ProfilesConfig>>,
    write_lock: Mutex<()>,
}

/// The persisted collection of application profiles.
#[derive(Clone)]
pub struct Profiles(Arc<ProfilesInner>);

impl Profiles {
    pub(crate) async fn load(context: Context) -> Result<Self> {
        let state = match next_config::load(&context.directories().profiles()).await {
            Ok(state) => state,
            Err(next_config::error::Error::Io(error))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                ProfilesConfig::default()
            }
            Err(error) => return Err(error.into()),
        };
        Ok(Self::new(context, state))
    }

    fn new(context: Context, state: ProfilesConfig) -> Self {
        let (published, _) = watch::channel(Arc::new(state));
        Self(Arc::new(ProfilesInner {
            context,
            published,
            write_lock: Mutex::new(()),
        }))
    }

    /// Returns every profile in unspecified order.
    pub fn list(&self) -> Vec<Profile> {
        self.0.published.borrow().profiles.clone()
    }

    /// Returns the selected profile, if the selection names an existing profile.
    pub fn selected(&self) -> Option<Profile> {
        let state = self.0.published.borrow();
        state.selected.and_then(|id| state.profile(id).cloned())
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
            state.selected = Some(id);
            Ok(profile)
        })
        .await
    }

    /// Clears the current profile selection.
    pub async fn clear_selection(&self) -> Result<()> {
        self.update(|state| {
            state.selected = None;
            Ok(())
        })
        .await
    }

    /// Deletes an existing profile, clearing the selection when necessary.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        self.update(move |state| {
            let index = state
                .profiles
                .iter()
                .position(|profile| profile.id == id)
                .ok_or(ProfileError::NotFound(id))?;
            state.profiles.remove(index);
            if state.selected == Some(id) {
                state.selected = None;
            }
            Ok(())
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

/// An immutable application-profile snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Profile {
    id: Uuid,
    name: String,
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
}
