use std::sync::Arc;

use async_trait::async_trait;
use bottles_plugin_host::{Plugin, PluginInfo, Plugins, WasiState};
use wasmtime::component::{Accessor, HasSelf, Resource};

use crate::{
    AccountIdentity, AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount,
};

mod bindings {
    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "account",
        imports: { default: trappable },
        exports: { default: async | store },
        with: {
            "bottles:plugin/account-link":
                crate::plugin::bottles::plugin::account_link,
        },
    });
}

use crate::plugin::bottles::plugin::account_link;

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
    crate::plugin::load_plugin(plugins, info, |store, instance| {
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
        self.call(move |accessor, bindings| {
            Box::pin(async move {
                let interaction =
                    accessor.with(|mut access| access.get().table.push(interaction))?;
                let borrowed = Resource::new_borrow(interaction.rep());
                let result = bindings
                    .bottles_plugin_account_provider()
                    .call_link_account(accessor, borrowed)
                    .await?;
                accessor.with(|mut access| access.get().table.delete(interaction))?;
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
