//! Watches `profiles.toml` for changes made by another process sharing
//! the same file (e.g. `next-server`'s `ProfileService` handling an RPC),
//! reloading and re-diffing against in-memory state so this process's
//! `ProfileManager::watch()` subscribers see external edits too.
//!
//! Runs on a plain OS thread — this crate stays executor-agnostic, so the
//! reload future is driven with `futures_lite::future::block_on` rather
//! than assuming a Tokio runtime is available. Only talks to
//! [`super::store`]'s free functions to reload/emit, the same primitives
//! `ProfileManager`'s own mutations use — this file's only job is
//! noticing an external change and figuring out what happened.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use next_proto::bottles::profiles::v1::ProfileEvent;
use notify::{RecursiveMode, Watcher};
use tokio::sync::{RwLock, broadcast};

use super::store::{self, ProfilesConfig};

/// Spawns the watcher thread. A no-op if `path` has no parent directory to
/// watch (shouldn't happen in practice — `path` always comes from
/// [`store::profiles_path`]).
pub(super) fn spawn(
    path: PathBuf,
    state: Arc<RwLock<ProfilesConfig>>,
    events: broadcast::Sender<ProfileEvent>,
) {
    let Some(watch_target) = path.parent().map(Path::to_path_buf) else {
        return;
    };

    thread::spawn(move || run(&path, &watch_target, &state, &events));
}

fn run(
    path: &Path,
    watch_target: &Path,
    state: &Arc<RwLock<ProfilesConfig>>,
    events: &broadcast::Sender<ProfileEvent>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let watched_path = path.to_path_buf();
    let mut watcher =
        match notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            if let Ok(event) = result
                && event.paths.iter().any(|changed| changed == &watched_path)
            {
                let _ = tx.send(());
            }
        }) {
            Ok(watcher) => watcher,
            Err(err) => {
                tracing::warn!("failed to start profiles.toml watcher: {err}");
                return;
            }
        };

    if let Err(err) = watcher.watch(watch_target, RecursiveMode::NonRecursive) {
        tracing::warn!("failed to watch {}: {err}", watch_target.display());
        return;
    }

    for () in rx {
        reconcile(path, state, events);
    }
}

/// Reloads `path`, replaces the in-memory state, and emits whatever
/// events the reload implies — a changed or newly-created profile is
/// `Updated`, a profile that disappeared is `DeletedProfileId`, and a
/// changed `active_profile_id` is `Activated`.
fn reconcile(
    path: &Path,
    state: &Arc<RwLock<ProfilesConfig>>,
    events: &broadcast::Sender<ProfileEvent>,
) {
    let (reloaded, old_profiles, old_active) = {
        let mut guard = futures_lite::future::block_on(state.write());
        let Ok(reloaded) = futures_lite::future::block_on(store::load(path)) else {
            return;
        };
        let old_profiles = std::mem::replace(&mut guard.profiles, reloaded.profiles.clone());
        let old_active = std::mem::replace(
            &mut guard.active_profile_id,
            reloaded.active_profile_id.clone(),
        );
        (reloaded, old_profiles, old_active)
    };

    for profile in &reloaded.profiles {
        let changed = old_profiles
            .iter()
            .find(|existing| existing.id == profile.id)
            .is_none_or(|existing| existing != profile);

        if changed {
            store::emit_updated(events, profile);
        }
    }

    for old in &old_profiles {
        if !reloaded.profiles.iter().any(|profile| profile.id == old.id) {
            store::emit_deleted(events, &old.id);
        }
    }

    if reloaded.active_profile_id != old_active
        && let Some(active) = reloaded.active()
    {
        store::emit_activated(events, &active);
    }
}
