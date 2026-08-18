//! Local install-state persistence for game library installs. Actually
//! downloading and writing a game's files is `download-manager`'s job
//! (via its chunked-download mode); this module only tracks what was
//! installed where, so it can be looked up and later uninstalled.

use std::path::PathBuf;

use next_proto::bottles::common::v1::Storefront;
use tokio::sync::RwLock;

use crate::{
    Bottle,
    error::Result,
    library::install::{InstallsConfig, installs_path},
};

pub mod install;

pub use install::{InstallRecord, install_root_relative_path, sanitize_relative_path};

pub struct LibraryManager {
    path: PathBuf,
    state: RwLock<InstallsConfig>,
}

impl LibraryManager {
    pub async fn load() -> Result<Self> {
        let path = installs_path()?;
        let state = match next_config::load::<InstallsConfig>(&path).await {
            Ok(state) => state,
            Err(next_config::error::Error::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                InstallsConfig::default()
            }
            Err(err) => return Err(err.into()),
        };
        Ok(Self {
            path,
            state: RwLock::new(state),
        })
    }

    pub async fn get(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Option<InstallRecord> {
        self.state.read().await.get(profile_id, storefront, game_id)
    }

    async fn persist(&self, state: &InstallsConfig) -> Result<()> {
        next_config::save(&self.path, state).await?;
        Ok(())
    }

    pub async fn upsert(&self, record: InstallRecord) -> Result<()> {
        let mut state = self.state.write().await;
        state.upsert(record);
        self.persist(&state).await
    }

    /// Removes and returns the record, if any. Callers still need to
    /// delete the install directory themselves — this only updates the
    /// record.
    async fn remove(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
    ) -> Result<Option<InstallRecord>> {
        let mut state = self.state.write().await;
        let Some(record) = state.remove(profile_id, storefront, game_id) else {
            return Ok(None);
        };
        self.persist(&state).await?;
        Ok(Some(record))
    }

    /// Removes the install's whole directory (every game gets its own
    /// exclusive `install::install_root_relative_path`, so this can't
    /// touch any other game's files) and the registered launch
    /// `Program`, if any. A no-op if no such install is on record.
    pub async fn uninstall(
        &self,
        profile_id: &str,
        storefront: Storefront,
        game_id: &str,
        bottle: Option<Bottle>,
    ) -> Result<()> {
        let Some(record) = self.remove(profile_id, storefront, game_id).await? else {
            return Ok(());
        };

        if let Some(bottle) = bottle {
            let c_drive = bottle.c_drive_path();
            match install::install_root_relative_path(storefront, game_id) {
                Some(install_root) => {
                    let _ = tokio::fs::remove_dir_all(c_drive.join(install_root)).await;
                }
                None => {
                    tracing::warn!("skipping uninstall of suspicious game_id {game_id:?}");
                }
            }
            if let Some(program_id) = record.program_id.as_deref()
                && let Ok(program_uuid) = uuid::Uuid::parse_str(program_id)
            {
                let mut edit = bottle.edit();
                edit.remove_program(program_uuid);
                if let Err(err) = edit.commit().await {
                    tracing::warn!("failed to remove launch program for {game_id}: {err}");
                }
            }
        }

        Ok(())
    }
}
