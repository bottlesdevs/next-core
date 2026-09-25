use std::sync::Arc;

use async_trait::async_trait;
use bottles_plugin_host::{CompiledPlugin, Invocation, Plugin, WasiState};
use wasmtime::component::{Accessor, HasSelf, Linker, Resource};
use wasmtime_wasi::WasiCtx;

use crate::{
    AccountIdentity, AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount,
};

mod bindings {
    pub type Interaction = std::sync::Arc<dyn crate::AccountLinkInteraction>;

    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "account",
        imports: { default: trappable },
        exports: { default: async | store },
        with: {
            "bottles:plugin/account-link.interaction": Interaction,
        },
    });
}

use bindings::{bottles::plugin::account_link, exports::bottles::plugin::account_provider};

/// Adds core domain imports to a caller-owned linker using its WASI state.
/// Account interaction is granted only by passing a resource to a linking call.
pub fn add_to_linker<T: Send + 'static>(
    linker: &mut Linker<T>,
    state: fn(&mut T) -> &mut WasiState,
) -> wasmtime::Result<()> {
    account_link::add_to_linker::<_, HasSelf<WasiState>>(linker, state)
}

impl<T: Send + 'static> account_link::HostInteractionWithStore<T> for HasSelf<WasiState> {
    async fn request_input(
        accessor: &Accessor<T, Self>,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
        url: String,
        instructions: String,
    ) -> wasmtime::Result<Result<String, String>> {
        let interaction =
            accessor.with(|mut access| access.get().table.get(&interaction).cloned())?;
        let url = match url::Url::parse(&url) {
            Ok(url) => url,
            Err(error) => return Ok(Err(error.to_string())),
        };
        Ok(interaction.request_input(url, instructions).await)
    }
}

impl account_link::HostInteraction for WasiState {
    fn drop(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
    ) -> wasmtime::Result<()> {
        self.table.delete(interaction)?;
        Ok(())
    }
}

impl account_link::Host for WasiState {}

/// A persistent account-provider session driven by its caller.
/// Clones share guest state; opening another provider creates an independent session.
/// Poll opening and calls within a caller-owned Tokio runtime with I/O and time enabled.
pub type PluginAccountProvider = Plugin<WasiState, account_provider::Guest>;

/// Opens an account-provider session with the caller's WASI capabilities.
/// Runtime errors or dropped active calls close the session permanently.
pub async fn open_account_provider(
    plugin: Arc<CompiledPlugin>,
    wasi: WasiCtx,
) -> bottles_plugin_host::Result<PluginAccountProvider> {
    let mut linker = Linker::new(plugin.component().engine());
    bottles_plugin_host::add_to_linker(&mut linker)?;
    add_to_linker(&mut linker, |state| state)?;
    let pre = linker.instantiate_pre(plugin.component())?;
    let indices = account_provider::GuestIndices::new(&pre)?;
    let mut invocation = Invocation::new(&pre, WasiState::new(wasi)).await?;
    let guest = indices.load(&mut invocation.store, &invocation.instance)?;
    Ok(Plugin::new(plugin, invocation, guest))
}

#[async_trait]
impl AccountProvider for PluginAccountProvider {
    fn metadata(&self) -> AccountProviderInfo {
        AccountProviderInfo {
            id: self.info().manifest.id.clone(),
            name: self.info().manifest.name.clone().into(),
        }
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Result<LinkedAccount, String> {
        self.call(move |invocation, guest| {
            Box::pin(async move {
                let interaction = invocation.store.data_mut().table.push(interaction)?;
                let borrowed = Resource::new_borrow(interaction.rep());
                let result = invocation
                    .store
                    .run_concurrent(async move |accessor| {
                        guest.call_link_account(accessor, borrowed).await
                    })
                    .await??;
                invocation.store.data_mut().table.delete(interaction)?;
                Ok(result)
            })
        })
        .await
        .map_err(|error| error.to_string())?
        .map(|linked| LinkedAccount {
            identity: AccountIdentity {
                account_id: linked.identity.account_id,
                display_name: linked.identity.display_name,
            },
            credential: linked.credential,
        })
    }
}
