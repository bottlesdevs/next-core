//! Adapt the host's installed-library calls to core's provider contract.

use async_trait::async_trait;
use bottles_plugin_host::LoadedPlugin;

use super::LibraryEntry;
use crate::{
    Operation,
    error::{Error, Result},
};

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
