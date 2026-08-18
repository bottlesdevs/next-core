//! Profile lifecycle and configuration — persistence, local mutation, and
//! change notification for user profiles.
//!
//! Deliberately doesn't dial storefront plugins (Store.RefreshSession,
//! Store.CompleteLogin, Store.RevokeSession, ...) — that's a
//! multi-process orchestration concern belonging to whatever holds the
//! Registry connection (`next-server`'s `ProfileService`), not this
//! local persistence layer. Methods here take the *result* of that work
//! (a refreshed `LinkedAccount`, a completed login's account, etc.) and
//! apply it.

pub mod error;

use std::{path::PathBuf, sync::Arc};

use futures_core::Stream;
use next_config::Config;
use next_proto::bottles::{
    common::v1::LinkedAccount,
    profiles::v1::{ProfileEvent, SteamLink, UserProfile, profile_event},
};
use prost_wkt_types::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, broadcast};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::{bottle::error::BottleError, error::Result};
use error::ProfileError;

const EVENTS_CAPACITY: usize = 16;
const PROFILES_FILE: &str = "profiles.toml";

fn now() -> Timestamp {
    Timestamp::from(std::time::SystemTime::now())
}

fn not_found(profile_id: &str) -> crate::error::Error {
    ProfileError::NotFound(profile_id.to_string()).into()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Config)]
#[config(version = 1)]
struct ProfilesConfig {
    active_profile_id: Option<String>,
    profiles: Vec<UserProfile>,
}

fn profiles_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| dirs.config_dir().join(PROFILES_FILE))
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

pub struct ProfileManager {
    path: PathBuf,
    state: Arc<RwLock<ProfilesConfig>>,
    events: broadcast::Sender<ProfileEvent>,
}

impl ProfileManager {
    pub async fn load() -> Result<Self> {
        let path = profiles_path()?;
        let state = match next_config::load::<ProfilesConfig>(&path).await {
            Ok(state) => state,
            Err(next_config::error::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                ProfilesConfig::default()
            }
            Err(err) => return Err(err.into()),
        };
        let (events, _) = broadcast::channel(EVENTS_CAPACITY);
        Ok(Self {
            path,
            state: Arc::new(RwLock::new(state)),
            events,
        })
    }

    async fn persist(&self, state: &ProfilesConfig) -> Result<()> {
        next_config::save(&self.path, state).await?;
        Ok(())
    }

    fn emit_updated(&self, profile: &UserProfile) {
        let _ = self.events.send(ProfileEvent {
            event: Some(profile_event::Event::Updated(profile.clone())),
        });
    }

    fn emit_activated(&self, profile: &UserProfile) {
        let _ = self.events.send(ProfileEvent {
            event: Some(profile_event::Event::Activated(profile.clone())),
        });
    }

    fn emit_deleted(&self, profile_id: &str) {
        let _ = self.events.send(ProfileEvent {
            event: Some(profile_event::Event::DeletedProfileId(
                profile_id.to_string(),
            )),
        });
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
            .ok_or_else(|| not_found(profile_id))
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
            Err(not_found(profile_id))
        }
    }

    pub async fn create(&self, name: String, icon: String) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = UserProfile {
            id: Uuid::new_v4().to_string(),
            name,
            icon,
            accounts: Vec::new(),
            steam_link: None,
            created_at: Some(now()),
            last_activated_at: None,
        };
        state.profiles.push(profile.clone());
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    pub async fn delete(&self, profile_id: &str) -> Result<()> {
        let mut state = self.state.write().await;
        let len_before = state.profiles.len();
        state.profiles.retain(|profile| profile.id != profile_id);
        if state.profiles.len() == len_before {
            return Err(not_found(profile_id));
        }
        if state.active_profile_id.as_deref() == Some(profile_id) {
            state.active_profile_id = None;
        }
        self.persist(&state).await?;
        self.emit_deleted(profile_id);
        Ok(())
    }

    pub async fn rename(&self, profile_id: &str, name: String) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        profile.name = name;
        let profile = profile.clone();
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    pub async fn active(&self) -> Option<UserProfile> {
        let state = self.state.read().await;
        state.active_profile_id.as_deref().and_then(|id| {
            state
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
        })
    }

    /// Every linked account eligible for activation, filtered to `only`
    /// when non-empty. Callers refresh each of these against its owning
    /// Store plugin, then report the outcomes to
    /// [`apply_activation`](Self::apply_activation).
    pub async fn accounts_for_activation(
        &self,
        profile_id: &str,
        only: &[i32],
    ) -> Result<Vec<LinkedAccount>> {
        let state = self.state.read().await;
        let profile = state
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        Ok(profile
            .accounts
            .iter()
            .filter(|account| only.is_empty() || only.contains(&account.storefront))
            .cloned()
            .collect())
    }

    /// Applies the outcome of refreshing each targeted account (`Ok` to
    /// replace it, `Err` to mark it stale in place), marks the profile
    /// active, and stamps `last_activated_at`.
    pub async fn apply_activation(
        &self,
        profile_id: &str,
        updates: std::collections::HashMap<i32, std::result::Result<LinkedAccount, ()>>,
    ) -> Result<UserProfile> {
        let mut updates = updates;
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;

        for account in &mut profile.accounts {
            match updates.remove(&account.storefront) {
                Some(Ok(refreshed)) => *account = refreshed,
                Some(Err(())) => {
                    account.auth_state =
                        next_proto::bottles::common::v1::AuthState::Stale as i32;
                }
                None => {}
            }
        }
        profile.last_activated_at = Some(now());
        let profile = profile.clone();

        state.active_profile_id = Some(profile_id.to_string());
        self.persist(&state).await?;
        self.emit_activated(&profile);
        Ok(profile)
    }

    pub async fn unlink_account(&self, profile_id: &str, storefront: i32) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        profile
            .accounts
            .retain(|account| account.storefront != storefront);
        let profile = profile.clone();
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    /// Attaches `account` to the profile (replacing any existing account
    /// for the same storefront), after a caller-completed login. Doesn't
    /// perform the login itself — see the module docs.
    pub async fn link_account(&self, profile_id: &str, account: LinkedAccount) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        profile
            .accounts
            .retain(|existing| existing.storefront != account.storefront);
        profile.accounts.push(account);
        let profile = profile.clone();
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    pub async fn link_steam(&self, profile_id: &str, steam_link: SteamLink) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        profile.steam_link = Some(steam_link);
        let profile = profile.clone();
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    pub async fn unlink_steam(&self, profile_id: &str) -> Result<UserProfile> {
        let mut state = self.state.write().await;
        let profile = state
            .profiles
            .iter_mut()
            .find(|profile| profile.id == profile_id)
            .ok_or_else(|| not_found(profile_id))?;
        profile.steam_link = None;
        let profile = profile.clone();
        self.persist(&state).await?;
        self.emit_updated(&profile);
        Ok(profile)
    }

    /// The current active profile (if any) as an initial `Activated`
    /// event, then every subsequent mutation. Subscribes before reading
    /// the initial state so no event lands in the gap between
    /// snapshotting "current" and listening for "next".
    pub fn watch(&self) -> impl Stream<Item = ProfileEvent> + Send + 'static {
        // Subscribe before reading state so no event can land in the gap
        // between snapshotting "current" and starting to listen for
        // "next".
        let receiver = self.events.subscribe();
        let state = self.state.clone();

        let initial = async move {
            let state = state.read().await;
            state.active_profile_id.as_deref().and_then(|id| {
                state
                    .profiles
                    .iter()
                    .find(|profile| profile.id == id)
                    .cloned()
            })
        };

        // A lagged receiver just means this subscriber missed some
        // events under backpressure — skip the gap rather than erroring
        // the whole stream out from under the caller.
        let live = BroadcastStream::new(receiver).filter_map(|item| item.ok());

        futures_util::stream::once(initial)
            .filter_map(|profile| {
                profile.map(|profile| ProfileEvent {
                    event: Some(profile_event::Event::Activated(profile)),
                })
            })
            .chain(live)
    }
}
