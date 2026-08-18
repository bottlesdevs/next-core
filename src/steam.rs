//! Reads Steam's local session state directly from `loginusers.vdf`.
//!
//! Pure local file parsing/watching — no network or proto types, same as
//! every other module here. `next-server`'s `ProfileService` maps
//! [`SteamUser`] to `bottles.steam.v1.SteamSessionEvent` itself.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    thread,
};

use futures_core::Stream;
use notify::{RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

/// A user entry parsed out of Steam's `loginusers.vdf`.
#[derive(Debug, Clone)]
pub struct SteamUser {
    pub steam_id64: String,
    pub account_name: String,
    /// Steam only ever tracks one locally logged-in user at a time; this
    /// mirrors the `MostRecent` flag it stores per entry.
    pub is_active: bool,
}

/// Locates `loginusers.vdf` across the install layouts Steam is commonly
/// found in (native and Flatpak on Linux, native on macOS). Returns the
/// first path that actually exists.
pub fn loginusers_vdf_path() -> Option<PathBuf> {
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

pub fn parse_loginusers(path: &Path) -> std::io::Result<Vec<SteamUser>> {
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

/// Looks up a single user's account name by SteamID64. Best-effort: any
/// I/O or parse failure (Steam not installed, file briefly mid-write,
/// unrecognized entry) just yields `None` rather than an error, since a
/// missing account name shouldn't block linking the account.
pub fn account_name_for(steam_id64: &str) -> Option<String> {
    let path = loginusers_vdf_path()?;
    let users = parse_loginusers(&path).ok()?;
    users
        .into_iter()
        .find(|user| user.steam_id64 == steam_id64)
        .map(|user| user.account_name)
}

/// Watches `loginusers.vdf` and emits a [`SteamUser`] whenever the
/// `MostRecent` (i.e. locally active) user changes. Runs the notify
/// watcher on a dedicated OS thread — notify's callback API is sync, and
/// this bridges it into the async world via an mpsc channel. The thread
/// exits on its own once the stream's consumer drops the receiver, since
/// the blocking `send` then fails.
pub fn watch_active_user() -> Pin<Box<dyn Stream<Item = SteamUser> + Send + 'static>> {
    let Some(path) = loginusers_vdf_path() else {
        tracing::debug!("no Steam installation found, Steam session watch will stay idle");
        return Box::pin(tokio_stream::empty());
    };

    let (tx, rx) = mpsc::channel(16);

    thread::spawn(move || {
        let watch_target = path.parent().unwrap_or(&path).to_path_buf();
        let mut last_active: Option<String> = None;

        let mut report = {
            let tx = tx.clone();
            let path = path.clone();
            move || {
                let Ok(users) = parse_loginusers(&path) else {
                    return;
                };
                let Some(active) = users.into_iter().find(|user| user.is_active) else {
                    return;
                };
                if last_active.as_deref() == Some(active.steam_id64.as_str()) {
                    return;
                }
                last_active = Some(active.steam_id64.clone());

                let _ = tx.blocking_send(active);
            }
        };

        report();

        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<_>| {
            if res.is_ok() {
                report();
            }
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!("failed to start Steam session watcher: {err}");
                return;
            }
        };

        if let Err(err) = watcher.watch(&watch_target, RecursiveMode::NonRecursive) {
            tracing::warn!("failed to watch {}: {err}", watch_target.display());
            return;
        }

        // This thread's only remaining job is to keep `watcher` alive —
        // notify delivers events via its own internal thread and calls
        // the closure above directly. Poll for the consumer dropping the
        // stream so the watcher (and this thread) eventually exits
        // instead of leaking for the lifetime of the process.
        while !tx.is_closed() {
            thread::park_timeout(std::time::Duration::from_secs(5));
        }
    });

    Box::pin(ReceiverStream::new(rx))
}
