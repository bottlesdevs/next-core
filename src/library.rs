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
///
/// # Examples
///
/// ```
/// use bottles_core::Library;
///
/// let library = Library::default();
/// assert!(futures_lite::future::block_on(library.list())?.is_empty());
/// # Ok::<(), bottles_core::error::Error>(())
/// ```
#[derive(Clone, Default)]
pub struct Library {
    providers: Arc<RwLock<HashMap<String, Arc<dyn LibraryProvider>>>>,
}

impl Library {
    /// Registers a provider, replacing the provider with the same [`LibraryProvider::id`].
    ///
    /// Call after installing or reloading a plugin to register its loaded handle.
    /// Existing items retain their original provider; refresh to obtain new items.
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use bottles_core::{Library, LibraryProvider};
    /// # fn register(library: &Library, provider: Arc<dyn LibraryProvider>) {
    /// library.register_provider(provider);
    /// # }
    /// ```
    pub fn register_provider(&self, provider: Arc<dyn LibraryProvider>) {
        let id = provider.id().to_owned();
        self.providers.write().unwrap().insert(id, provider);
    }

    /// Removes a provider from subsequent listings.
    ///
    /// Previously returned [`LibraryItem`] values keep their provider handle and
    /// can still launch through it.
    /// Plugin unloading or reloading separately retires its old host handles.
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::Library;
    ///
    /// let library = Library::default();
    /// library.remove_provider("retired-plugin");
    /// ```
    pub fn remove_provider(&self, provider_id: &str) {
        self.providers.write().unwrap().remove(provider_id);
    }

    /// Queries every registered provider and combines its current entries.
    ///
    /// Providers are captured before enumeration; registration changes affect the
    /// next listing. Ordering is unspecified. The first provider error is returned.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by [`LibraryProvider::list_entries`].
    ///
    /// # Panics
    ///
    /// Panics if a previous writer poisoned the provider lock.
    ///
    /// # Examples
    ///
    /// ```
    /// use bottles_core::Library;
    ///
    /// let library = Library::default();
    /// let items = futures_lite::future::block_on(library.list())?;
    /// assert!(items.is_empty());
    /// # Ok::<(), bottles_core::error::Error>(())
    /// ```
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
///
/// # Examples
///
/// ```
/// # use bottles_core::LibraryItem;
/// # fn print_item(item: &LibraryItem) {
/// println!("{}: {}", item.provider_id(), item.entry().title);
/// # }
/// ```
#[derive(Clone)]
pub struct LibraryItem {
    entry: LibraryEntry,
    provider: Arc<dyn LibraryProvider>,
}

impl LibraryItem {
    /// Returns the metadata captured during listing.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::LibraryItem;
    /// # fn title(item: &LibraryItem) -> &str {
    /// &item.entry().title
    /// # }
    /// ```
    pub fn entry(&self) -> &LibraryEntry {
        &self.entry
    }

    /// Returns the stable identifier of the source provider.
    ///
    /// Native providers use `bottles` or `programs`; plugins use their manifest
    /// identifier.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::LibraryItem;
    /// # fn source(item: &LibraryItem) -> &str {
    /// item.provider_id()
    /// # }
    /// ```
    pub fn provider_id(&self) -> &str {
        self.provider.id()
    }

    /// Resolves the entry through its original provider and prepares its launch.
    ///
    /// The returned [`Operation`] is lazy and only submits the launch when polled.
    ///
    /// # Errors
    ///
    /// Returns [`Error::LibraryProvider`] when the provider rejects or no longer
    /// recognizes the stored entry identifier. Native providers may also return
    /// environment or identifier errors.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{LibraryItem, Operation};
    /// # fn prepare(item: &LibraryItem) -> Result<Operation<()>, bottles_core::error::Error> {
    /// let launch = item.launch()?;
    /// # Ok(launch)
    /// # }
    /// ```
    ///
    /// [`Error::LibraryProvider`]: crate::error::Error::LibraryProvider
    pub fn launch(&self) -> Result<Operation<()>> {
        self.provider.launch(&self.entry.id)
    }
}

/// Supplies installed entries and resolves launches for one source.
///
/// Entry IDs are local to this provider. Enumeration and launch run only while
/// their caller drives the returned future or operation.
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
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::LibraryProvider;
    /// # fn provider_name(provider: &dyn LibraryProvider) -> &str {
    /// provider.id()
    /// # }
    /// ```
    fn id(&self) -> &str;

    /// Returns entries available from the provider's current state.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error if current entries cannot be enumerated.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::LibraryProvider;
    /// # async fn count(provider: &dyn LibraryProvider) -> Result<usize, bottles_core::error::Error> {
    /// let entries = provider.list_entries().await?;
    /// # Ok(entries.len())
    /// # }
    /// ```
    async fn list_entries(&self) -> Result<Vec<LibraryEntry>>;

    /// Resolves an entry and prepares its launch without starting it.
    /// The operation completes when the launch request finishes, not when the title exits.
    ///
    /// # Errors
    ///
    /// Returns an error if `entry_id` is malformed, unknown, or cannot be resolved
    /// against the provider's current state.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::{LibraryProvider, Operation};
    /// # fn prepare(provider: &dyn LibraryProvider, id: &str) -> Result<Operation<()>, bottles_core::error::Error> {
    /// provider.launch(id)
    /// # }
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
