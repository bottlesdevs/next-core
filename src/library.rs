//! Aggregation of launchable entries from core and plugin providers.
//!
//! [`Library`] is an explicitly refreshed view: each call to [`Library::list`]
//! asks every currently registered [`LibraryProvider`] for its latest entries.

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

/// Registers providers and combines their launchable entries.
///
/// Clones share provider registrations. This collection owns neither provider
/// storage nor background refresh tasks.
#[derive(Clone, Default)]
pub struct Library {
    providers: Arc<RwLock<HashMap<String, Arc<dyn LibraryProvider>>>>,
}

impl Library {
    /// Registers a provider, replacing the provider with the same [`LibraryProvider::id`].
    ///
    /// Previously returned [`LibraryItem`] values retain their original provider
    /// handle; call [`list`](Self::list) again to obtain entries from the replacement.
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
    pub fn register_provider(&self, provider: Arc<dyn LibraryProvider>) {
        let id = provider.id().to_owned();
        self.providers.write().unwrap().insert(id, provider);
    }

    /// Removes a provider from subsequent listings.
    ///
    /// Previously returned [`LibraryItem`] values keep their provider handle and
    /// can still launch through it.
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
    pub fn remove_provider(&self, provider_id: &str) {
        self.providers.write().unwrap().remove(provider_id);
    }

    /// Queries every registered provider and combines its current entries.
    ///
    /// Providers are captured before the first query, so concurrent registration
    /// changes affect only later listings. Providers are queried sequentially in
    /// unspecified order, and enumeration stops at the first error. Item order is
    /// therefore also unspecified.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by [`LibraryProvider::list_entries`].
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
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

/// A listed entry paired with the provider that can launch it.
///
/// The metadata is a snapshot from the last [`Library::list`] call. Listing
/// again is the only way to refresh it.
#[derive(Clone)]
pub struct LibraryItem {
    entry: LibraryEntry,
    provider: Arc<dyn LibraryProvider>,
}

impl LibraryItem {
    /// Returns the metadata captured during listing.
    pub fn entry(&self) -> &LibraryEntry {
        &self.entry
    }

    /// Returns the stable identifier of the source provider.
    ///
    /// Native providers use `bottles` or `programs`; plugins use their manifest
    /// identifier.
    pub fn provider_id(&self) -> &str {
        self.provider.id()
    }

    /// Resolves the entry through its original provider and prepares its launch.
    ///
    /// Resolution happens immediately. The returned [`Operation`] is lazy and
    /// submits the resolved launch only when polled; completion means the launch
    /// request finished, not that the title exited.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LibraryProvider`] when the provider rejects or no longer
    /// recognizes the stored entry identifier. Native providers may also return
    /// environment or identifier errors.
    ///
    /// [`Error::LibraryProvider`]: crate::error::Error::LibraryProvider
    pub fn launch(&self) -> Result<Operation<()>> {
        self.provider.launch(&self.entry.id)
    }
}

/// Supplies installed entries and resolves launches for one source.
///
/// Entry IDs are local to this provider, and launch operations are lazy.
/// Implementations must keep [`id`](Self::id) stable while registered.
///
/// # Examples
///
/// ```
/// use bottles_core::{LibraryEntry, LibraryProvider, Operation};
/// use bottles_core::error::{Error, Result};
///
/// struct EmptyProvider;
///
/// #[async_trait::async_trait]
/// impl LibraryProvider for EmptyProvider {
///     fn id(&self) -> &str { "empty" }
///
///     async fn list_entries(&self) -> Result<Vec<LibraryEntry>> { Ok(Vec::new()) }
///
///     fn launch(&self, _entry_id: &str) -> Result<Operation<()>> {
///         Err(Error::LibraryProvider {
///             provider: self.id().into(),
///             message: "entry not found".into(),
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait LibraryProvider: Send + Sync {
    /// Returns the stable registration key for this provider.
    fn id(&self) -> &str;

    /// Returns entries available from the provider's current state.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error if current entries cannot be enumerated.
    async fn list_entries(&self) -> Result<Vec<LibraryEntry>>;

    /// Resolves an entry and prepares its launch without starting it.
    /// The operation completes when the launch request finishes, not when the title exits.
    ///
    /// # Errors
    ///
    /// Returns an error if `entry_id` is malformed, unknown, or cannot be resolved
    /// against the provider's current state.
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
