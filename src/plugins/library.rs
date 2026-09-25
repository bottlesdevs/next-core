use std::sync::Arc;

use async_trait::async_trait;
use bottles_plugin_host::{CompiledPlugin, Invocation, Plugin, WasiState};
use wasmtime::component::Linker;
use wasmtime_wasi::WasiCtx;

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

use bindings::exports::bottles::plugin::library_provider;

/// A persistent installed-title provider driven by its caller.
/// Clones share guest state; opening another provider creates an independent session.
/// Poll opening and calls within a caller-owned Tokio runtime with I/O and time enabled.
pub type PluginLibraryProvider = Plugin<WasiState, library_provider::Guest>;

/// Opens a library-provider session with the caller's WASI capabilities.
/// Runtime errors or dropped active calls close the session permanently.
pub async fn open_library_provider(
    plugin: Arc<CompiledPlugin>,
    wasi: WasiCtx,
) -> bottles_plugin_host::Result<PluginLibraryProvider> {
    let mut linker = Linker::new(plugin.component().engine());
    bottles_plugin_host::add_to_linker(&mut linker)?;
    super::add_to_linker(&mut linker, |state| state)?;
    let pre = linker.instantiate_pre(plugin.component())?;
    let indices = library_provider::GuestIndices::new(&pre)?;
    let mut invocation = Invocation::new(&pre, WasiState::new(wasi)).await?;
    let guest = indices.load(&mut invocation.store, &invocation.instance)?;
    Ok(Plugin::new(plugin, invocation, guest))
}

#[async_trait]
impl LibraryProvider for PluginLibraryProvider {
    fn id(&self) -> &str {
        &self.info().manifest.id
    }

    async fn list_entries(&self) -> Result<Vec<LibraryEntry>> {
        self.call(move |invocation, guest| {
            Box::pin(async move {
                invocation
                    .store
                    .run_concurrent(async move |accessor| guest.call_list_entries(accessor).await)
                    .await?
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
                .run_until_cancelled(provider.call(move |invocation, guest| {
                    Box::pin(async move {
                        invocation
                            .store
                            .run_concurrent(async move |accessor| {
                                guest.call_launch(accessor, entry_id).await
                            })
                            .await?
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
