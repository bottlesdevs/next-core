//! Native Steam account discovery. Callers decide how observations affect profiles.

use std::{borrow::Cow, io, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures_core::Stream;
use futures_util::{StreamExt, stream};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::watch;
use tokio_stream::wrappers::WatchStream;
use tokio_util::sync::CancellationToken;

use super::{
    AccountIdentity, AccountLinkInteraction, LinkedAccount, StorefrontAccountProvider,
    StorefrontProvider,
};
use crate::PluginId;

pub const PROVIDER_ID: PluginId = PluginId::new("steam");
pub(super) const METADATA: StorefrontProvider = StorefrontProvider {
    id: PROVIDER_ID,
    name: Cow::Borrowed("Steam"),
};

#[cfg(target_os = "macos")]
const LOGINUSERS_PATHS: &[&str] = &["Library/Application Support/Steam/config/loginusers.vdf"];

#[cfg(target_os = "linux")]
const LOGINUSERS_PATHS: &[&str] = &[
    ".steam/steam/config/loginusers.vdf",
    ".local/share/Steam/config/loginusers.vdf",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam/config/loginusers.vdf",
];

/// Stateless native account-link adapter; observation has a separate caller-owned lifetime.
pub(super) struct SteamIntegration;

/// Observe the initial local Steam account and subsequent session-file changes.
/// Starts when polled and drops its native watcher with the stream. Changes may coalesce.
/// If Steam is absent, yields `Ok(None)` and ends; watch setup failure yields an error.
/// Reading and parsing occur on the caller's executor, never in the native callback.
pub fn watch_account() -> impl Stream<Item = io::Result<Option<AccountIdentity>>> + Send + 'static {
    stream::once(async {
        let Some(path) = loginusers_path() else {
            return stream::once(async { Ok(None) }).boxed();
        };
        let (changes, receiver) = watch::channel(Ok(()));
        let watcher = match watch_loginusers(path.clone(), changes) {
            Ok(watcher) => watcher,
            Err(error) => return stream::once(async move { Err(io::Error::other(error)) }).boxed(),
        };
        // Install observation before the initial read to retain changes during that read.
        WatchStream::new(receiver)
            .then(move |change| {
                let _keep_alive = &watcher;
                let path = path.clone();
                async move {
                    change.map_err(io::Error::other)?;
                    parse_active_account(&async_fs::read_to_string(path).await?)
                }
            })
            .boxed()
    })
    .flatten()
}

#[async_trait]
impl StorefrontAccountProvider for SteamIntegration {
    fn metadata(&self) -> StorefrontProvider {
        METADATA
    }

    async fn link_account(
        &self,
        _interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> Result<LinkedAccount, String> {
        let identity = cancellation
            .run_until_cancelled(active_account())
            .await
            .ok_or_else(|| "account linking cancelled".to_owned())?
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Steam has no active local account".to_owned())?;
        Ok(LinkedAccount {
            identity,
            credential: None,
        })
    }
}

fn watch_loginusers(
    path: PathBuf,
    changes: watch::Sender<Result<(), String>>,
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
                let _ = changes.send_replace(Ok(()));
            }
            Ok(_) => {}
            Err(error) => {
                let _ = changes.send_replace(Err(error.to_string()));
            }
        })?;
    watcher.watch(&directory, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Read Steam's most recent local account without observing or selecting profiles.
pub async fn active_account() -> io::Result<Option<AccountIdentity>> {
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
