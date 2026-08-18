use std::path::{Component, Path, PathBuf};

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
    pub bottle_id: String,
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
        let storefront = Storefront::try_from(record.storefront).unwrap_or(Storefront::Unspecified);
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

/// Rejects anything that isn't made entirely of normal path segments:
/// absolute paths, `..`, and Windows drive prefixes are all refused
/// rather than silently stripped, since callers join the result directly
/// onto a bottle's `C:` drive.
pub fn sanitize_relative_path(path: &str) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(part) => result.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!result.as_os_str().is_empty()).then_some(result)
}

pub fn installs_path() -> Result<PathBuf> {
    directories::ProjectDirs::from("com", "usebottles", "bottles-next")
        .map(|dirs| dirs.config_dir().join(INSTALLS_FILE))
        .ok_or_else(|| BottleError::ProjectDirectoriesUnavailable.into())
}

/// Where a game's files are written, relative to its bottle's `C:` drive
/// — `Program Files/<storefront>/<game_id>`.
pub fn install_root_relative_path(storefront: Storefront, game_id: &str) -> Option<PathBuf> {
    let game_id_path =
        sanitize_relative_path(game_id).filter(|path| path.components().count() == 1)?;
    Some(
        PathBuf::from("Program Files")
            .join(storefront.as_str_name())
            .join(game_id_path),
    )
}
