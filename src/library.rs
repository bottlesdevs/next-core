//! Installed, launchable entries supplied by native and plugin providers.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub use bottles_plugin_host::LibraryEntry;

use crate::{
    Operation,
    error::{Error, Result},
};
use async_trait::async_trait;
use bottles_plugin_host::LoadedPlugin;

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

/// A source of installed, launchable entries.
///
/// Entry IDs are local to this provider. Enumeration and launch run only while
/// their caller drives the returned future or operation.
#[async_trait]
pub trait LibraryProvider: Send + Sync {
    /// Stable registration key, independent of the entries currently available.
    fn id(&self) -> &str;

    /// Returns the entries available from the provider's current state.
    async fn list_entries(&self) -> Result<Vec<LibraryEntry>>;

    /// Resolves an entry and prepares its launch without starting it.
    /// The operation completes when the launch request finishes, not when the title exits.
    fn launch(&self, entry_id: &str) -> Result<Operation<()>>;
}

#[async_trait]
impl LibraryProvider for LoadedPlugin {
    fn id(&self) -> &str {
        &self.info.manifest.id
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        bottles_plugin_host::library::list_entries(self)
            .await
            .map_err(|message| Error::LibraryProvider {
                provider: self.id().to_owned(),
                message,
            })
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let plugin = self.clone();
        let entry_id = entry_id.to_owned();
        // Operation checks cancellation before submission. Once accepted by the
        // host, await the reply even if cancellation is requested in the meantime.
        Ok(Operation::new(move |_, _| async move {
            bottles_plugin_host::library::launch(&plugin, &entry_id)
                .await
                .map_err(|message| Error::LibraryProvider {
                    provider: plugin.id().to_owned(),
                    message,
                })
        }))
    }
}
