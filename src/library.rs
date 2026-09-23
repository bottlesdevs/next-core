//! Installed, launchable entries supplied by native and plugin providers.

mod plugin;

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub use bottles_plugin_host::LibraryEntry;

use crate::{Operation, error::Result};
pub use plugin::LibraryProvider;

/// An explicitly refreshed collection of installed, launchable entries.
///
/// Clones share provider registrations. This collection owns neither provider
/// storage nor background refresh tasks.
#[derive(Clone, Default)]
pub struct Library {
    providers: Arc<RwLock<HashMap<String, Arc<dyn LibraryProvider>>>>,
}

impl Library {
    /// Registers a provider, replacing any registration with the same ID.
    ///
    /// Call after installing or reloading a plugin to register its loaded handle.
    /// Existing items retain their original provider; refresh to obtain new items.
    pub fn register_provider(&self, provider: Arc<dyn LibraryProvider>) {
        let id = provider.id().to_owned();
        self.providers.write().unwrap().insert(id, provider);
    }

    /// Removes a provider from subsequent listings without invalidating existing items.
    /// Plugin unloading or reloading separately retires its old host handles.
    pub fn remove_provider(&self, provider_id: &str) {
        self.providers.write().unwrap().remove(provider_id);
    }

    /// Queries each registered provider and combines its current entries.
    ///
    /// Providers are captured before enumeration; registration changes affect the
    /// next listing. Ordering is unspecified. The first provider error is returned.
    pub async fn list(&self) -> Result<Vec<LibraryItem>> {
        let providers = self
            .providers
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut items = Vec::new();
        for provider in providers {
            items.extend(
                provider
                    .list_entries()
                    .await?
                    .into_iter()
                    .map(|entry| LibraryItem {
                        entry,
                        provider: provider.clone(),
                    }),
            );
        }
        Ok(items)
    }
}

/// Display metadata captured during listing, with launch bound to its original provider.
#[derive(Clone)]
pub struct LibraryItem {
    entry: LibraryEntry,
    provider: Arc<dyn LibraryProvider>,
}

impl LibraryItem {
    /// Returns the metadata captured during listing; list again to refresh it.
    pub fn entry(&self) -> &LibraryEntry {
        &self.entry
    }

    /// Returns the source key: `bottles`, `programs`, or the plugin's manifest ID.
    pub fn provider_id(&self) -> &str {
        self.provider.id()
    }

    /// Resolves the entry through its provider and prepares a caller-driven launch.
    pub fn launch(&self) -> Result<Operation<()>> {
        self.provider.launch(&self.entry.id)
    }
}
