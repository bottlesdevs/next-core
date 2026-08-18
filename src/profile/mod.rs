//! Persisted application profiles and selection.

pub mod error;
mod store;
mod watcher;

use std::{path::PathBuf, sync::Arc};

use futures_core::Stream;
use next_proto::bottles::profiles::v1::{ProfileEvent, UserProfile, profile_event};
use tokio::sync::{RwLock, broadcast};
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::error::Result;
use store::ProfilesConfig;

const EVENTS_CAPACITY: usize = 16;

#[derive(Clone)]
pub struct ProfileManager {
    path: PathBuf,
    config: Arc<RwLock<ProfilesConfig>>,
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
            config: state,
            events,
        })
    }

    pub async fn list(&self) -> Vec<UserProfile> {
        self.config.read().await.profiles.clone()
    }

    pub async fn get(&self, profile_id: &str) -> Result<UserProfile> {
        self.config
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

    pub fn watch_active_profile(&self) -> impl Stream<Item = ProfileEvent> + Send + 'static {
        let receiver = self.events.subscribe();
        let state = self.config.clone();

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

    pub async fn active(&self) -> Option<UserProfile> {
        self.config.read().await.active()
    }

    async fn mutate<T>(&self, op: impl FnOnce(&mut ProfilesConfig) -> Result<T>) -> Result<T> {
        let mut state = self.config.write().await;
        let value = op(&mut state)?;
        store::persist(&self.path, &state).await?;
        Ok(value)
    }
}
