use std::sync::Arc;

use async_trait::async_trait;
use bottles_plugin_host::{Plugin, PluginInfo, Plugins, WasiState};
use wasmtime::component::{Accessor, HasSelf, Linker, Resource};

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

use bindings::bottles::plugin::account_link;

/// Adds account imports to the host linker using its WASI state.
/// Account interaction is granted only by passing a resource to a linking call.
pub(crate) fn add_to_linker(linker: &mut Linker<WasiState>) -> wasmtime::Result<()> {
    account_link::add_to_linker::<_, HasSelf<WasiState>>(linker, |state| state)
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
pub(super) type PluginAccountProvider = Plugin<WasiState, bindings::Account>;

/// Opens an independent account-provider session from the installed catalog.
pub(super) async fn open_account_provider(
    plugins: &Plugins,
    info: &PluginInfo,
) -> bottles_plugin_host::Result<PluginAccountProvider> {
    plugins
        .load(info, add_to_linker, |store, instance| {
            bindings::Account::new(store, instance)
        })
        .await
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
        self.call(move |store, bindings| {
            Box::pin(async move {
                let interaction = store.data_mut().table.push(interaction)?;
                let borrowed = Resource::new_borrow(interaction.rep());
                let result = store
                    .run_concurrent(async move |accessor| {
                        bindings
                            .bottles_plugin_account_provider()
                            .call_link_account(accessor, borrowed)
                            .await
                    })
                    .await??;
                store.data_mut().table.delete(interaction)?;
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
