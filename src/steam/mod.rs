//! Native Steam account discovery and profile selection.

mod utils;

use std::sync::Arc;

use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use uuid::{NonNilUuid, Uuid};

use crate::{
    AccountIdentity, Profiles, StorefrontAccountProvider, StorefrontProvider,
    steam::utils::{SteamUser, active_user, loginusers_vdf_path},
};

const STEAM_PROVIDER_NAMESPACE: &str = "https://usebottles.com/providers/steam";

pub(crate) fn provider_id() -> NonNilUuid {
    NonNilUuid::new(Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        STEAM_PROVIDER_NAMESPACE.as_bytes(),
    ))
    .expect("a UUID-v5 is never nil")
}

struct SteamAccountProvider;

#[async_trait]
impl StorefrontAccountProvider for SteamAccountProvider {
    fn provider(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: provider_id(),
            name: "Steam".into(),
        }
    }

    async fn link_account(&self, _profile_id: Uuid) -> Result<AccountIdentity, String> {
        let user = blocking::unblock(active_user)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Steam has no active local account".to_owned())?;
        Ok(AccountIdentity {
            display_name: display_name(&user),
            account_id: user.steam_id64,
        })
    }
}

/// Keeps Steam's native provider and local-session watcher alive.
pub(crate) struct SteamIntegration {
    _watcher: Option<RecommendedWatcher>,
}

impl SteamIntegration {
    pub(crate) async fn open(profiles: Profiles) -> Self {
        profiles.register_builtin_account_provider(Arc::new(SteamAccountProvider));

        let Some(path) = loginusers_vdf_path() else {
            tracing::debug!("Steam is not installed; session observation is disabled");
            return Self { _watcher: None };
        };

        let initial = active_user_at(&path);
        if let Ok(Some(user)) = &initial {
            select_profile(&profiles, user).await;
        }
        let mut last_active = initial.ok().flatten().map(|user| user.steam_id64);
        let observed_path = path.clone();
        let watched_profiles = profiles.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if event.is_err() {
                return;
            }
            let Ok(Some(user)) = active_user_at(&observed_path) else {
                return;
            };
            if last_active.as_deref() == Some(user.steam_id64.as_str()) {
                return;
            }
            last_active = Some(user.steam_id64.clone());
            futures_lite::future::block_on(select_profile(&watched_profiles, &user));
        })
        .and_then(|mut watcher| {
            watcher.watch(path.parent().unwrap_or(&path), RecursiveMode::NonRecursive)?;
            Ok(watcher)
        })
        .inspect_err(|error| tracing::warn!("failed to observe Steam sessions: {error}"))
        .ok();

        Self { _watcher: watcher }
    }
}

fn active_user_at(path: &std::path::Path) -> std::io::Result<Option<SteamUser>> {
    utils::parse_loginusers(path).map(|users| users.into_iter().find(|user| user.is_active))
}

async fn select_profile(profiles: &Profiles, user: &SteamUser) {
    if let Err(error) = profiles
        .select_account(provider_id(), &user.steam_id64)
        .await
    {
        tracing::warn!(account_id = %user.steam_id64, "failed to select Steam profile: {error}");
    }
}

fn display_name(user: &SteamUser) -> String {
    if user.account_name.is_empty() {
        user.steam_id64.clone()
    } else {
        user.account_name.clone()
    }
}
