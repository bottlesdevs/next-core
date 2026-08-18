//! Reads Steam's local session state directly from `loginusers.vdf`.
//!
//! Pure local file parsing/watching — no network or proto types, same as
//! every other module here. `next-server`'s `ProfileService` maps
//! [`SteamUser`] to `bottles.steam.v1.SteamSessionEvent` itself.

use futures_core::Stream;
use next_proto::bottles::steam::v1::{SteamLink, SteamSessionEvent};
use tokio_stream::StreamExt;

use crate::{
    error::{ProfileError, Result},
    profile::ProfileManager,
    steam::utils::watch_active_user,
};

mod error;
mod utils;
pub use error::SteamError;
pub use utils::{SteamUser, account_name_for, loginusers_vdf_path, parse_loginusers};

/// A local Steam session manager that reads `loginusers.vdf` and watches for
/// changes to the active user.
pub struct SteamManager {
    profile: ProfileManager,
}

impl SteamManager {
    pub fn new(profile: ProfileManager) -> Self {
        Self { profile }
    }

    /// Links a Steam account by ID to `profile_id`. Looks up the display
    /// name from the local Steam install's `loginusers.vdf` on a
    /// best-effort basis (empty if Steam isn't installed or the ID isn't
    /// found there).
    pub async fn link_account(&self, profile_id: &str, steam_link: SteamLink) -> Result<SteamLink> {
        let Ok(mut profile) = self.profile.get(profile_id).await else {
            return Err(ProfileError::NotFound(profile_id.into()).into());
        };
        if profile
            .steam_link
            .as_ref()
            .is_some_and(|link| link.steam_id64 == steam_link.steam_id64)
        {
            return Err(SteamError::SteamAccountAlreadyLinked {
                steam_id64: steam_link.steam_id64,
                linked_profile_name: profile.name.clone(),
            }
            .into());
        }
        profile.steam_link = Some(steam_link.clone());
        self.profile.update(profile).await?;
        Ok(steam_link)
    }

    pub async fn unlink_account(&self, profile_id: &str) -> Result<()> {
        let mut profile = self.profile.get(profile_id).await?;
        profile.steam_link = None;
        self.profile.update(profile).await
    }

    /// Fires whenever the OS-level Steam active user changes (via
    /// filesystem watch on `loginusers.vdf`).
    pub fn watch_sessions(&self) -> impl Stream<Item = SteamSessionEvent> + Send + 'static {
        watch_active_user().map(|user| SteamSessionEvent {
            steam_id64: user.steam_id64,
            account_name: user.account_name,
            is_active: user.is_active,
        })
    }
}
