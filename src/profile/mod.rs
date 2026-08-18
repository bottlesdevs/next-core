//! Persisted application profiles and selection.

pub mod error;
mod store;
mod watcher;

use std::{path::PathBuf, sync::Arc};

use futures_core::Stream;
use next_proto::bottles::{
    common::v1::LinkedAccount,
    profiles::v1::{ProfileEvent, UserProfile, profile_event},
    steam::v1::SteamLink,
};
use tokio::sync::{RwLock, broadcast};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::error::Result;
use error::ProfileError;
use store::ProfilesConfig;

const EVENTS_CAPACITY: usize = 16;

#[derive(Clone)]
pub struct ProfileManager {
    path: PathBuf,
    state: Arc<RwLock<ProfilesConfig>>,
    events: broadcast::Sender<ProfileEvent>,
}

impl ProfileManager {
    pub async fn new() -> Result<Self> {
        let path = store::profiles_path()?;
        let state = Arc::new(RwLock::new(store::load(&path).await?));
        let (events, _) = broadcast::channel(EVENTS_CAPACITY);

        watcher::spawn(path.clone(), state.clone(), events.clone());

        Ok(Self {
            path,
            state,
            events,
        })
    }

    pub async fn list(&self) -> Vec<UserProfile> {
        self.state.read().await.profiles.clone()
    }

    pub async fn get(&self, profile_id: &str) -> Result<UserProfile> {
        self.state
            .read()
            .await
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .cloned()
            .ok_or_else(|| store::not_found(profile_id))
    }

    pub async fn create(&self, name: String, icon: String) -> Result<UserProfile> {
        let profile = UserProfile {
            id: Uuid::new_v4().to_string(),
            name,
            icon,
            accounts: Vec::new(),
            steam_link: None,
            created_at: Some(store::now()),
            last_activated_at: None,
        };
        let profile = self
            .mutate(|state| {
                state.profiles.push(profile.clone());
                Ok(profile)
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    pub async fn delete(&self, profile_id: &str) -> Result<()> {
        self.mutate(|state| {
            let len_before = state.profiles.len();
            state.profiles.retain(|profile| profile.id != profile_id);
            if state.profiles.len() == len_before {
                return Err(store::not_found(profile_id));
            }
            if state.active_profile_id.as_deref() == Some(profile_id) {
                state.active_profile_id = None;
            }
            Ok(())
        })
        .await?;
        store::emit_deleted(&self.events, profile_id);
        Ok(())
    }

    pub async fn rename(&self, profile_id: &str, name: String) -> Result<UserProfile> {
        let profile = self
            .mutate(|state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;

                profile.name = name;
                Ok(profile.clone())
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    pub async fn update(&self, profile: UserProfile) -> Result<()> {
        let profile_id = &profile.id;
        self.mutate(|state| {
            let profile = state
                .profiles
                .iter_mut()
                .find(|p| p.id == *profile_id)
                .ok_or_else(|| store::not_found(profile_id))?;
            *profile = profile.clone();
            Ok(())
        })
        .await?;
        store::emit_updated(&self.events, &profile);
        Ok(())
    }

    pub async fn active(&self) -> Option<UserProfile> {
        self.state.read().await.active()
    }

    /// Marks `profile_id` as the active profile and stamps
    /// `last_activated_at`. Callers that need to refresh linked accounts
    /// first should do so via [`Self::update`] before calling this.
    pub async fn activate(&self, profile_id: &str) -> Result<UserProfile> {
        let profile = self
            .mutate(|state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;

                profile.last_activated_at = Some(store::now());
                let profile = profile.clone();

                state.active_profile_id = Some(profile_id.to_string());
                Ok(profile)
            })
            .await?;
        store::emit_activated(&self.events, &profile);
        Ok(profile)
    }

    /// The current active profile (if any) as an initial `Activated`
    /// event, then every subsequent mutation. Subscribes before reading
    /// the initial state so no event lands in the gap between
    /// snapshotting "current" and listening for "next".
    pub fn watch_active_profile(&self) -> impl Stream<Item = ProfileEvent> + Send + 'static {
        let receiver = self.events.subscribe();
        let state = self.state.clone();

        let initial = async move { state.read().await.active() };

        let live = BroadcastStream::new(receiver).filter_map(|item| item.ok());

        futures_util::stream::once(initial)
            .filter_map(|profile| {
                profile.map(|profile| ProfileEvent {
                    event: Some(profile_event::Event::Activated(profile)),
                })
            })
            .chain(live)
    }

    pub async fn unlink_account(&self, profile_id: &str, storefront: i32) -> Result<UserProfile> {
        let profile = self
            .mutate(|state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;
                profile
                    .accounts
                    .retain(|account| account.storefront != storefront);
                Ok(profile.clone())
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    /// Attaches `account` to the profile (replacing any existing account
    /// for the same storefront), after a caller-completed login.
    pub async fn link_account(
        &self,
        profile_id: &str,
        account: LinkedAccount,
    ) -> Result<UserProfile> {
        let profile = self
            .mutate(move |state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;
                profile
                    .accounts
                    .retain(|existing| existing.storefront != account.storefront);
                profile.accounts.push(account);
                Ok(profile.clone())
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    /// Links `steam_link` to `profile_id`, refusing if that Steam account is
    /// already linked to a *different* profile — a Steam account maps to
    /// one real person, so it shouldn't be claimable by more than one local
    /// profile at a time. Re-linking the same account to the same profile
    /// (e.g. to refresh `account_name`) is allowed.
    pub async fn link_steam(&self, profile_id: &str, steam_link: SteamLink) -> Result<UserProfile> {
        let profile = self
            .mutate(move |state| {
                if let Some(existing) = state.profiles.iter().find(|profile| {
                    profile.id != profile_id
                        && profile
                            .steam_link
                            .as_ref()
                            .is_some_and(|link| link.steam_id64 == steam_link.steam_id64)
                }) {
                    return Err(ProfileError::SteamAccountAlreadyLinked {
                        steam_id64: steam_link.steam_id64,
                        linked_profile_name: existing.name.clone(),
                    }
                    .into());
                }

                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;
                profile.steam_link = Some(steam_link);
                Ok(profile.clone())
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    pub async fn unlink_steam(&self, profile_id: &str) -> Result<UserProfile> {
        let profile = self
            .mutate(|state| {
                let profile = state
                    .profiles
                    .iter_mut()
                    .find(|profile| profile.id == profile_id)
                    .ok_or_else(|| store::not_found(profile_id))?;
                profile.steam_link = None;
                Ok(profile.clone())
            })
            .await?;
        store::emit_updated(&self.events, &profile);
        Ok(profile)
    }

    /// Fails fast if the profile doesn't exist, without cloning it.
    /// Useful before starting a network operation that shouldn't be
    /// attempted for a nonexistent profile.
    pub async fn ensure_exists(&self, profile_id: &str) -> Result<()> {
        if self
            .state
            .read()
            .await
            .profiles
            .iter()
            .any(|profile| profile.id == profile_id)
        {
            Ok(())
        } else {
            Err(store::not_found(profile_id))
        }
    }

    async fn mutate<T>(&self, op: impl FnOnce(&mut ProfilesConfig) -> Result<T>) -> Result<T> {
        let mut state = self.state.write().await;
        let value = op(&mut state)?;
        store::persist(&self.path, &state).await?;
        Ok(value)
    }
}
