//! The persisted config shape and the primitives shared by
//! `ProfileManager`'s mutations and [`super::watcher`]'s external-file
//! reconciliation: loading, saving, and emitting the [`ProfileEvent`]s
//! subscribers see. Kept free of both the write-lock orchestration
//! (`ProfileManager::mutate`) and the file-watching thread, so this file
//! is just "what's in a profiles.toml and how do we persist/announce it."

use std::path::{Path, PathBuf};

use next_config::Config;
use next_proto::bottles::profiles::v1::{ProfileEvent, UserProfile, profile_event};
use prost_wkt_types::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::error::ProfileError;
use crate::{bottle::error::BottleError, error::Result};

const PROFILES_FILE: &str = "profiles.toml";

pub(super) fn now() -> Timestamp {
    Timestamp::from(std::time::SystemTime::now())
}

pub(super) fn not_found(profile_id: &str) -> crate::error::Error {
    ProfileError::NotFound(profile_id.to_string()).into()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, Config)]
#[config(version = 1)]
pub(super) struct ProfilesConfig {
    pub active_profile_id: Option<String>,
    pub profiles: Vec<UserProfile>,
}

impl ProfilesConfig {
    pub(super) fn active(&self) -> Option<UserProfile> {
        self.active_profile_id.as_deref().and_then(|id| {
            self.profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
        })
    }
}

pub(super) fn profiles_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| dirs.config_dir().join(PROFILES_FILE))
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

pub(super) async fn load(path: &Path) -> Result<ProfilesConfig> {
    match next_config::load::<ProfilesConfig>(path).await {
        Ok(state) => Ok(state),
        Err(next_config::error::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(ProfilesConfig::default())
        }
        Err(err) => Err(err.into()),
    }
}

pub(super) async fn persist(path: &Path, state: &ProfilesConfig) -> Result<()> {
    next_config::save(path, state).await?;
    Ok(())
}

pub(super) fn emit_updated(events: &broadcast::Sender<ProfileEvent>, profile: &UserProfile) {
    let _ = events.send(ProfileEvent {
        event: Some(profile_event::Event::Updated(profile.clone())),
    });
}

pub(super) fn emit_activated(events: &broadcast::Sender<ProfileEvent>, profile: &UserProfile) {
    let _ = events.send(ProfileEvent {
        event: Some(profile_event::Event::Activated(profile.clone())),
    });
}

pub(super) fn emit_deleted(events: &broadcast::Sender<ProfileEvent>, profile_id: &str) {
    let _ = events.send(ProfileEvent {
        event: Some(profile_event::Event::DeletedProfileId(
            profile_id.to_string(),
        )),
    });
}
