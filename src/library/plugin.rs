use async_trait::async_trait;
use bottles_plugin_host::{Plugin, PluginInfo, Plugins, WasiState};

use crate::{
    LibraryEntry, LibraryProvider, Operation,
    error::{Error, Result},
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "library",
        exports: { default: async | store },
    });
}

/// A persistent installed-title provider driven by its caller.
/// Clones share guest state; opening another provider creates an independent session.
/// Poll opening and calls within a caller-owned Tokio runtime with I/O and time enabled.
pub(super) type PluginLibraryProvider = Plugin<WasiState, bindings::Library>;

/// Opens an independent library-provider session from the installed catalog.
pub(super) async fn open_library_provider(
    plugins: &Plugins,
    info: &PluginInfo,
) -> bottles_plugin_host::Result<PluginLibraryProvider> {
    crate::plugin::load_plugin(plugins, info, |store, instance| {
        bindings::Library::new(store, instance)
    })
    .await
}

#[async_trait]
impl LibraryProvider for PluginLibraryProvider {
    fn id(&self) -> &str {
        &self.info().manifest.id
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        self.call(move |accessor, bindings| {
            Box::pin(async move {
                bindings
                    .bottles_plugin_library_provider()
                    .call_list_entries(accessor)
                    .await
            })
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result)
        .map(|entries| {
            entries
                .into_iter()
                .map(|entry| LibraryEntry {
                    id: entry.id,
                    title: entry.title,
                })
                .collect()
        })
        .map_err(|message| Error::LibraryProvider {
            provider: self.id().to_owned(),
            message,
        })
    }

    fn launch(&self, entry_id: &str) -> Result<Operation<()>> {
        let provider = self.clone();
        let entry_id = entry_id.to_owned();
        Ok(Operation::new(move |_, cancellation| async move {
            cancellation
                .run_until_cancelled(provider.call(move |accessor, bindings| {
                    Box::pin(async move {
                        bindings
                            .bottles_plugin_library_provider()
                            .call_launch(accessor, entry_id)
                            .await
                    })
                }))
                .await
                .ok_or(Error::Cancelled)?
                .map_err(|error| error.to_string())
                .and_then(|result| result)
                .map_err(|message| Error::LibraryProvider {
                    provider: provider.id().to_owned(),
                    message,
                })
        }))
    }
}
