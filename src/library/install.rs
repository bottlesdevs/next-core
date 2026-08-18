use std::path::PathBuf;

use crate::error::{BottleError, Result};
use next_config::Config;
use next_proto::bottles::common::v1::{InstallState, Storefront};
use serde::{Deserialize, Serialize};

const INSTALLS_FILE: &str = "installs.toml";

#[derive(Debug, Default, Clone, Serialize, Deserialize, Config)]
#[config(version = 1)]
pub struct InstallsConfig {
    installs: Vec<InstallRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallRecord {
    pub profile_id: String,
    pub storefront: i32,
    pub game_id: String,
    pub version: String,
    pub install_size_bytes: Option<u64>,
    /// Which bottle's `C:` drive these files were written into.
    pub bottle_id: String,
    /// Paths installed relative to that bottle's `C:` drive, recorded
    /// at install time so uninstalling removes exactly what was
    /// written even if the manifest changes later.
    pub relative_paths: Vec<String>,
    /// The `Program` registered on the bottle for this install's launch
    /// executable, if one was found — so uninstalling can remove it
    /// too. Unset when no primary executable could be determined.
    pub program_id: Option<String>,
}

impl InstallRecord {
    fn matches(&self, profile_id: &str, storefront: Storefront, game_id: &str) -> bool {
        self.profile_id == profile_id
            && self.storefront == storefront as i32
            && self.game_id == game_id
    }

    pub fn install_state(&self) -> InstallState {
        InstallState {
            installed: true,
            bottle_id: Some(self.bottle_id.clone()),
            installed_version: Some(self.version.clone()),
            install_size_bytes: self.install_size_bytes,
        }
    }
}

impl InstallsConfig {
    pub fn get(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Option<InstallRecord> {
        self.installs
            .iter()
            .find(|record| record.matches(profile_id, storefront, game_id))
            .cloned()
    }

    /// Replaces any existing record for the same (profile, storefront,
    /// game) with `record`.
    pub fn upsert(&mut self, record: InstallRecord) {
        let storefront =
            Storefront::try_from(record.storefront).unwrap_or(Storefront::Unspecified);
        self.installs
            .retain(|existing| !existing.matches(&record.profile_id, storefront, &record.game_id));
        self.installs.push(record);
    }

    /// Removes and returns the matching record, if any.
    pub fn remove(
        &mut self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Option<InstallRecord> {
        let index = self
            .installs
            .iter()
            .position(|record| record.matches(profile_id, storefront, game_id))?;
        Some(self.installs.remove(index))
    }
}

pub fn installs_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| dirs.config_dir().join(INSTALLS_FILE))
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

/// Where a game's files are written. Not part of `InstallsConfig` itself
/// since it's derived, not persisted.
pub fn install_dir(profile_id: &str, storefront: Storefront, game_id: &str) -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| {
            dirs.data_dir()
                .join("installs")
                .join(profile_id)
                .join(storefront.as_str_name())
                .join(game_id)
        })
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}
