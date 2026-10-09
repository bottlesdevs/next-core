//! Aggregation of launchable entries from registered providers.
//!
//! [`Library::watch`] follows provider registrations and their entry streams, and
//! [`Library::launch`] delegates to the currently registered provider.

use std::{collections::HashMap, sync::Arc};

#[cfg(feature = "fvs")]
use crate::Program;
use crate::{
    Bottle, Manager, Operation,
    error::{Error, Result},
    utils::join::join,
};
use futures_core::Stream;
use futures_util::{StreamExt, stream::BoxStream};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use uuid::Uuid;

/// An installed title supplied by a library provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LibraryEntry {
    /// Identifier local to the provider.
    pub id: String,
    /// Display title.
    pub title: String,
}

/// A provider's latest listing.
#[derive(Debug)]
pub enum ProviderState {
    /// The provider has not finished its first listing.
    Loading,
    /// The provider's latest available entries.
    Loaded(Vec<LibraryEntry>),
    /// The provider's latest enumeration error.
    Failed(Error),
}

/// An immutable map of all registered providers and their latest listing states.
///
/// Provider states are shared across snapshots without cloning entries or errors.
pub type LibrarySnapshot = Arc<HashMap<String, Arc<ProviderState>>>;

/// Registers providers and combines their launchable entries.
///
/// Clones share provider registrations.
#[derive(Clone, Default)]
pub struct Library {
    providers: watch::Sender<Arc<HashMap<String, Arc<dyn LibraryProvider>>>>,
}

impl Library {
    /// Registers a provider, replacing the provider with the same [`LibraryProvider::id`].
    ///
    /// Watchers observe the replacement's published state and stop watching the old provider.
    pub fn register_provider(&self, provider: Arc<dyn LibraryProvider>) {
        let id = provider.id().to_owned();
        self.providers.send_modify(|providers| {
            Arc::make_mut(providers).insert(id, provider);
        });
    }

    /// Removes a provider from subsequent listings.
    ///
    /// Watchers stop watching this provider and omit it from their next snapshot.
    pub fn remove_provider(&self, provider_id: &str) {
        self.providers.send_modify(|providers| {
            Arc::make_mut(providers).remove(provider_id);
        });
    }

    /// Yields an initial snapshot, then follows registrations and entry updates.
    /// Registry changes resubscribe providers to their current published states.
    /// Each snapshot retains the latest state per provider and omits removed
    /// providers. The stream ends when the last [`Library`] sharing this registry
    /// is dropped, even if providers remain alive.
    pub fn watch(&self) -> impl Stream<Item = LibrarySnapshot> + Send + 'static + use<> {
        join(self.providers.subscribe(), |provider| provider.entries())
    }

    /// Asks the currently registered provider to prepare an entry's launch.
    ///
    /// The provider may validate the entry here or defer validation to the
    /// returned [`Operation`]. The operation is lazy, and its completion means
    /// the launch request finished, not that the title exited.
    ///
    /// # Errors
    ///
    /// Returns an error if the provider is not registered or cannot prepare the launch.
    /// Awaiting the returned operation may fail later if deferred validation or
    /// launch fails.
    pub fn launch(&self, provider: &str, entry: &str) -> Result<Operation<()>> {
        let registered = self
            .providers
            .borrow()
            .get(provider)
            .cloned()
            .ok_or_else(|| Error::LibraryProvider {
                provider: provider.to_owned(),
                message: "provider not found".into(),
            })?;
        registered.launch(entry)
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
/// use bottles_core::{LibraryProvider, Operation, ProviderState};
/// use bottles_core::error::{Error, Result};
/// use futures_util::{StreamExt, stream::{self, BoxStream}};
/// use std::{future::ready, sync::Arc};
///
/// struct EmptyProvider;
///
/// impl LibraryProvider for EmptyProvider {
///     fn id(&self) -> &str { "empty" }
///
///     fn entries(&self) -> BoxStream<'static, Arc<ProviderState>> {
///         stream::once(ready(Arc::new(ProviderState::Loaded(Vec::new())))).boxed()
///     }
///
///     fn launch(&self, _entry_id: &str) -> Result<Operation<()>> {
///         Err(Error::LibraryProvider {
///             provider: self.id().into(),
///             message: "entry not found".into(),
///         })
///     }
/// }
/// ```
pub trait LibraryProvider: Send + Sync {
    /// Returns the stable registration key for this provider.
    fn id(&self) -> &str;

    /// Observes the provider's own published listing state.
    ///
    /// The current item must be ready when first polled. Subscribing must be cheap
    /// and starts no work. Providers publish [`ProviderState::Loading`]
    /// until their first listing completes, then publish loaded entries or an error.
    /// Each item replaces the previous state; a finished stream retains its last
    /// published state in [`Library::watch`].
    fn entries(&self) -> BoxStream<'static, Arc<ProviderState>>;

    /// Prepares a lazy launch for an entry.
    ///
    /// Implementations may validate `entry_id` here or when the returned
    /// operation is polled. The operation completes when the launch request
    /// finishes, not when the title exits.
    ///
    /// # Errors
    ///
    /// Returns an error if the launch cannot be prepared immediately. Deferred
    /// validation and launch failures are returned by the operation.
    fn launch(&self, entry_id: &str) -> Result<Operation<()>>;
}

impl LibraryProvider for Manager<Bottle> {
    fn id(&self) -> &str {
        "bottles"
    }

    fn entries(&self) -> BoxStream<'static, Arc<ProviderState>> {
        self.watch()
            .map(|states| {
                let mut entries = Vec::new();
                for state in states {
                    entries.extend(state.programs().map(|(id, program)| LibraryEntry {
                        id: format!("{}/{id}", state.id()),
                        title: program.name().to_owned(),
                    }));
                }
                Arc::new(ProviderState::Loaded(entries))
            })
            .boxed()
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let (bottle_id, program_id) =
            entry_id
                .split_once('/')
                .ok_or_else(|| Error::LibraryProvider {
                    provider: self.id().to_owned(),
                    message: "expected bottle UUID/program UUID".into(),
                })?;
        let parse_id = |id| {
            Uuid::parse_str(id).map_err(|error| Error::LibraryProvider {
                provider: self.id().to_owned(),
                message: error.to_string(),
            })
        };
        Ok(self
            .open(parse_id(bottle_id)?)?
            .launch_program(parse_id(program_id)?)
            .map(|_| ()))
    }
}

#[cfg(feature = "fvs")]
impl LibraryProvider for Manager<Program> {
    fn id(&self) -> &str {
        "programs"
    }

    fn entries(&self) -> BoxStream<'static, Arc<ProviderState>> {
        self.watch()
            .map(|states| {
                Arc::new(ProviderState::Loaded(
                    states
                        .iter()
                        .map(|state| LibraryEntry {
                            id: state.id().to_string(),
                            title: state.name().to_owned(),
                        })
                        .collect(),
                ))
            })
            .boxed()
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let id = Uuid::parse_str(entry_id).map_err(|error| Error::LibraryProvider {
            provider: self.id().to_owned(),
            message: error.to_string(),
        })?;
        Ok(self.open(id)?.launch().map(|_| ()))
    }
}
