//! Built-in Steam account discovery.
//!
//! Linking reads Steam's local `loginusers.vdf` and selects the entry marked
//! `MostRecent`. It never changes Steam's active user and stores no credential.

use async_trait::async_trait;
use std::{borrow::Cow, io, path::PathBuf, sync::Arc};

use bottles_plugin_host::AccountIdentity;

use super::{AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount};

pub(super) fn metadata() -> AccountProviderInfo {
    AccountProviderInfo {
        id: "steam".into(),
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

/// Native provider that links Steam's most recently used local account.
///
/// The VDF account key becomes [`AccountIdentity::account_id`]. A non-empty
/// `AccountName` becomes [`AccountIdentity::display_name`]; otherwise the account
/// ID is used for both fields.
pub(super) struct Steam;

#[async_trait]
impl AccountProvider for Steam {
    fn metadata(&self) -> AccountProviderInfo {
        metadata()
    }

    async fn link_account(
        &self,
        _interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Result<LinkedAccount, String> {
        let identity = active_account()
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "Steam has no active local account".to_owned())?;
        Ok(LinkedAccount {
            identity,
            credential: None,
        })
    }
}

/// Reads Steam's most recent local account without changing Steam state.
///
/// Returns `None` when no supported Steam configuration path exists or no account
/// is marked as most recent.
///
/// # Errors
///
/// Returns an error if the first discovered configuration cannot be read or parsed.
async fn active_account() -> io::Result<Option<AccountIdentity>> {
    let Some(path) = loginusers_path() else {
        return Ok(None);
    };
    parse_active_account(&async_fs::read_to_string(path).await?)
}

/// Returns the first existing `loginusers.vdf` path in platform precedence order.
fn loginusers_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_owned();
    LOGINUSERS_PATHS
        .iter()
        .map(|path| home.join(path))
        .find(|path| path.exists())
}

/// Extracts the first account marked `MostRecent` from Steam VDF text.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] if the text is invalid VDF or its root
/// value is not an object.
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
