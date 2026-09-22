//! Native Steam account discovery during explicit account linking.

use async_trait::async_trait;
use std::{borrow::Cow, io, path::PathBuf, sync::Arc};

use super::wasm::{AccountIdentity, Authentication, OwnedGame};
use tokio_util::sync::CancellationToken;

use super::{AccountLinkInteraction, LinkedAccount, Provider, StorefrontProvider};

pub(super) fn metadata() -> StorefrontProvider {
    StorefrontProvider {
        id: "native:steam".into(),
        name: Cow::Borrowed("Steam"),
    }
}

#[cfg(target_os = "macos")]
const LOGINUSERS_PATHS: &[&str] = &["Library/Application Support/Steam/config/loginusers.vdf"];

#[cfg(target_os = "linux")]
const LOGINUSERS_PATHS: &[&str] = &[
    ".steam/steam/config/loginusers.vdf",
    ".local/share/Steam/config/loginusers.vdf",
    ".var/app/com.valvesoftware.Steam/.local/share/Steam/config/loginusers.vdf",
];

pub(super) struct Steam;

#[async_trait]
impl Provider for Steam {
    fn metadata(&self) -> StorefrontProvider {
        metadata()
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

    async fn authenticate(
        &self,
        _account_id: &str,
        _credential: Option<&[u8]>,
    ) -> Result<Authentication, String> {
        Ok(Authentication {
            access: Vec::new(),
            updated_credential: None,
        })
    }

    async fn list_games(
        &self,
        _account_id: &str,
        _access: &[u8],
        _cancellation: &CancellationToken,
    ) -> Result<Vec<OwnedGame>, String> {
        Ok(Vec::new())
    }
}

/// Read Steam's most recent local account without observing or selecting profiles.
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
