//! Native Steam account discovery and profile selection.

mod utils;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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
        profiles
            .register_account_provider(Arc::new(SteamAccountProvider))
            .expect("native Steam provider registration cannot be rejected");

        let Some(path) = loginusers_vdf_path() else {
            tracing::debug!("Steam is not installed; session observation is disabled");
            return Self { _watcher: None };
        };

        let mut last_active = None;
        handle_loginusers_change(&profiles, &path, &mut last_active).await;
        let watcher = watch_loginusers(path, profiles, last_active)
            .inspect_err(|error| tracing::warn!("failed to observe Steam sessions: {error}"))
            .ok();

        Self { _watcher: watcher }
    }
}

fn watch_loginusers(
    path: PathBuf,
    profiles: Profiles,
    mut last_active: Option<String>,
) -> notify::Result<RecommendedWatcher> {
    let directory = path.parent().unwrap_or(path.as_path()).to_owned();
    let observed_path = path.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                tracing::warn!("failed to observe Steam sessions: {error}");
                return;
            }
        };
        if !event_targets_loginusers(&event, &observed_path) {
            return;
        }
        futures_lite::future::block_on(handle_loginusers_change(
            &profiles,
            &observed_path,
            &mut last_active,
        ));
    })?;
    watcher.watch(&directory, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

fn event_targets_loginusers(event: &notify::Event, path: &Path) -> bool {
    event.need_rescan()
        || event
            .paths
            .iter()
            .any(|changed| changed == path || path.parent().is_some_and(|parent| changed == parent))
}

async fn handle_loginusers_change(
    profiles: &Profiles,
    path: &Path,
    last_active: &mut Option<String>,
) {
    let user = match active_user_at(path) {
        Ok(user) => user,
        Err(error) => {
            tracing::warn!(path = %path.display(), "failed to read Steam sessions: {error}");
            return;
        }
    };
    let Some(user) = user else {
        last_active.take();
        return;
    };
    if last_active.as_deref() == Some(user.steam_id64.as_str()) {
        return;
    }
    match profiles
        .select_account(provider_id(), &user.steam_id64)
        .await
    {
        Ok(_) => *last_active = Some(user.steam_id64),
        Err(error) => {
            tracing::warn!(account_id = %user.steam_id64, "failed to select Steam profile: {error}");
        }
    }
}

fn active_user_at(path: &Path) -> std::io::Result<Option<SteamUser>> {
    utils::parse_loginusers(path).map(|users| users.into_iter().find(|user| user.is_active))
}

fn display_name(user: &SteamUser) -> String {
    if user.account_name.is_empty() {
        user.steam_id64.clone()
    } else {
        user.account_name.clone()
    }
}
