//! The persisted config shape and the primitives shared by
//! `ProfileManager`'s mutations and [`super::watcher`]'s external-file
//! reconciliation: loading, saving, and diffing two [`ProfilesConfig`]
//! snapshots into the [`ProfileEvent`]s subscribers see.

use std::path::{Path, PathBuf};

use next_config::Config;
use next_proto::bottles::profiles::v1::{ProfileEvent, UserProfile, profile_event};
use prost_wkt_types::Timestamp;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use super::error::ProfileError;
use crate::{Directories, error::Result};

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

pub(super) fn profiles_path(directories: &Directories) -> PathBuf {
    directories.config_dir().join(PROFILES_FILE)
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

/// Diffs `old` against `new` and emits whatever [`ProfileEvent`]s the difference implies.
pub(super) fn diff_and_emit(
    events: &broadcast::Sender<ProfileEvent>,
    old: &ProfilesConfig,
    new: &ProfilesConfig,
) {
    for profile in &new.profiles {
        let changed = old
            .profiles
            .iter()
            .find(|existing| existing.id == profile.id)
            .is_none_or(|existing| existing != profile);

        if changed {
            let _ = events.send(ProfileEvent {
                event: Some(profile_event::Event::Updated(profile.clone())),
            });
        }
    }

    for old_profile in &old.profiles {
        if !new
            .profiles
            .iter()
            .any(|profile| profile.id == old_profile.id)
        {
            let _ = events.send(ProfileEvent {
                event: Some(profile_event::Event::DeletedProfileId(
                    old_profile.id.to_string(),
                )),
            });
        }
    }

    if new.active_profile_id != old.active_profile_id
        && let Some(active) = new.active()
    {
        let _ = events.send(ProfileEvent {
            event: Some(profile_event::Event::Activated(active.clone())),
        });
    }
}
