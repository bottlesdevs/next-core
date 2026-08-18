use std::path::{Path, PathBuf};

/// A user entry parsed out of Steam's `loginusers.vdf`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct SteamUser {
    pub steam_id64: String,
    pub account_name: String,
    /// Steam only ever tracks one locally logged-in user at a time; this
    /// mirrors the `MostRecent` flag it stores per entry.
    pub is_active: bool,
}

/// Locates `loginusers.vdf` across the install layouts Steam is commonly
/// found in (native and Flatpak on Linux, native on macOS). Returns the
/// first path that actually exists.
pub(crate) fn loginusers_vdf_path() -> Option<PathBuf> {
    let home = directories::BaseDirs::new()?.home_dir().to_path_buf();

    let candidates: &[PathBuf] = &if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/Steam/config/loginusers.vdf")]
    } else {
        vec![
            home.join(".steam/steam/config/loginusers.vdf"),
            home.join(".local/share/Steam/config/loginusers.vdf"),
            home.join(".var/app/com.valvesoftware.Steam/.local/share/Steam/config/loginusers.vdf"),
        ]
    };

    candidates.iter().find(|path| path.exists()).cloned()
}

pub(crate) fn parse_loginusers(path: &Path) -> std::io::Result<Vec<SteamUser>> {
    let text = std::fs::read_to_string(path)?;
    let Ok(vdf) = keyvalues_parser::parse(&text) else {
        return Ok(Vec::new());
    };
    let Some(users) = vdf.value.get_obj() else {
        return Ok(Vec::new());
    };

    let mut out = Vec::new();
    for (steam_id64, values) in users.iter() {
        let Some(entry) = values.first().and_then(|value| value.get_obj()) else {
            continue;
        };

        let account_name = entry
            .get("AccountName")
            .and_then(|values| values.first())
            .and_then(|value| value.get_str())
            .unwrap_or_default()
            .to_string();

        let is_active = entry
            .get("MostRecent")
            .and_then(|values| values.first())
            .and_then(|value| value.get_str())
            == Some("1");

        out.push(SteamUser {
            steam_id64: steam_id64.to_string(),
            account_name,
            is_active,
        });
    }

    Ok(out)
}

pub(crate) fn active_user() -> std::io::Result<Option<SteamUser>> {
    let Some(path) = loginusers_vdf_path() else {
        return Ok(None);
    };
    parse_loginusers(&path).map(|users| users.into_iter().find(|user| user.is_active))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_login_users_and_most_recent_account() {
        let directory =
            std::env::temp_dir().join(format!("bottles-steam-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("loginusers.vdf");
        std::fs::write(
            &path,
            r#""users"
{
    "76561198000000000"
    {
        "AccountName" "first"
        "MostRecent" "0"
    }
    "76561198000000001"
    {
        "AccountName" "second"
        "MostRecent" "1"
    }
}"#,
        )
        .unwrap();

        let users = parse_loginusers(&path).unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(
            users.into_iter().find(|user| user.is_active).unwrap(),
            SteamUser {
                steam_id64: "76561198000000001".into(),
                account_name: "second".into(),
                is_active: true,
            }
        );

        std::fs::remove_dir_all(directory).unwrap();
    }
}
