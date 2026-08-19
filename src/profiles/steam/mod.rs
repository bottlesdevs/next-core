//! Native Steam account discovery and profile selection.

use std::{io, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use uuid::{NonNilUuid, Uuid};

use super::{AccountIdentity, ProfilesInner, StorefrontAccountProvider, StorefrontProvider};

const PROVIDER_ID: NonNilUuid =
    NonNilUuid::new(uuid::uuid!("a6a63a3c-d671-581a-9007-6a8a9c9a7da8"))
        .expect("Steam provider UUID is non-nil");

#[cfg(target_os = "macos")]
const LOGINUSERS_PATHS: &[&str] = &["Library/Application Support/Steam/config/loginusers.vdf"];

#[cfg(target_os = "linux")]
const LOGINUSERS_PATHS: &[&str] = &[
    ".steam/steam/config/loginusers.vdf",
    ".local/share/Steam/config/loginusers.vdf",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam/config/loginusers.vdf",
];

struct SteamAccountProvider;

#[async_trait]
impl StorefrontAccountProvider for SteamAccountProvider {
    fn provider(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: PROVIDER_ID,
            name: "Steam".into(),
        }
    }

    async fn link_account(&self, _profile_id: Uuid) -> Result<AccountIdentity, String> {
        active_account()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Steam has no active local account".to_owned())
    }
}

/// Keeps Steam's native provider and local-session watcher alive.
pub(super) struct SteamIntegration {
    _watcher: Option<RecommendedWatcher>,
}

impl SteamIntegration {
    pub(super) async fn open(profiles: Arc<ProfilesInner>) -> Self {
        profiles
            .register_account_provider(Arc::new(SteamAccountProvider))
            .expect("native Steam provider registration cannot be rejected");

        let watcher = match loginusers_path() {
            Some(path) => {
                let watcher = watch_loginusers(path, profiles.clone())
                    .inspect_err(|error| {
                        tracing::warn!("failed to observe Steam sessions: {error}")
                    })
                    .ok();
                select_active_profile(&profiles).await;
                watcher
            }
            None => {
                tracing::debug!("Steam is not installed; session observation is disabled");
                None
            }
        };

        Self { _watcher: watcher }
    }
}

fn watch_loginusers(
    path: PathBuf,
    profiles: Arc<ProfilesInner>,
) -> notify::Result<RecommendedWatcher> {
    let directory = path.parent().unwrap_or(path.as_path()).to_owned();
    let observed_directory = directory.clone();
    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<notify::Event>| match event {
            Ok(event)
                if event.need_rescan()
                    || event
                        .paths
                        .iter()
                        .any(|changed| changed == &path || changed == &observed_directory) =>
            {
                futures_lite::future::block_on(select_active_profile(&profiles));
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!("failed to observe Steam sessions: {error}");
            }
        })?;
    watcher.watch(&directory, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

async fn select_active_profile(profiles: &ProfilesInner) {
    let account = match active_account().await {
        Ok(account) => account,
        Err(error) => {
            tracing::warn!("failed to read Steam sessions: {error}");
            return;
        }
    };
    let Some(account) = account else {
        return;
    };
    if let Err(error) = profiles
        .select_account(PROVIDER_ID, &account.account_id)
        .await
    {
        tracing::warn!(account_id = %account.account_id, "failed to select Steam profile: {error}");
    }
}

async fn active_account() -> io::Result<Option<AccountIdentity>> {
    let Some(path) = loginusers_path() else {
        return Ok(None);
    };
    parse_active_account(&async_fs::read_to_string(path).await?)
}

fn loginusers_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_owned();
    LOGINUSERS_PATHS
        .iter()
        .map(|path| home.join(path))
        .find(|path| path.exists())
}

fn parse_active_account(text: &str) -> io::Result<Option<AccountIdentity>> {
    let vdf = keyvalues_parser::parse(text)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let users = vdf.value.get_obj().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "loginusers.vdf root is not an object",
        )
    })?;

    for (steam_id, values) in users.iter() {
        let Some(account) = values.first().and_then(|value| value.get_obj()) else {
            continue;
        };
        if field(account, "MostRecent") != Some("1") {
            continue;
        }

        let account_id = steam_id.to_string();
        let display_name = field(account, "AccountName")
            .filter(|name| !name.is_empty())
            .unwrap_or(&account_id)
            .to_owned();
        return Ok(Some(AccountIdentity {
            account_id,
            display_name,
        }));
    }

    Ok(None)
}

fn field<'a>(object: &'a keyvalues_parser::Obj<'_>, name: &str) -> Option<&'a str> {
    object.get(name)?.first()?.get_str()
}
