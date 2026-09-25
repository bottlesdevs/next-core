use std::sync::Arc;

use async_trait::async_trait;
use bottles_plugin_host::{CompiledPlugin, Invocation, Session, WasiState};
use wasmtime::component::{HasSelf, Linker, Resource};
use wasmtime_wasi::WasiCtx;

use crate::{
    AccountIdentity, AccountLinkInteraction, AccountProvider, AccountProviderInfo, LinkedAccount,
};

mod bindings {
    pub type Interaction = std::sync::Arc<dyn crate::AccountLinkInteraction>;

    wasmtime::component::bindgen!({
        path: "../next-plugin-api/wit",
        world: "account",
        imports: { default: async | trappable },
        exports: { default: async },
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

impl account_link::HostInteraction for WasiState {
    async fn request_input(
        &mut self,
        interaction: Resource<Arc<dyn AccountLinkInteraction>>,
        url: String,
        instructions: String,
    ) -> wasmtime::Result<Result<String, String>> {
        let interaction = self.table.get(&interaction)?.clone();
        let url = match url::Url::parse(&url) {
            Ok(url) => url,
            Err(error) => return Ok(Err(error.to_string())),
        };
        Ok(interaction.request_input(url, instructions).await)
    }

    async fn drop(
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
#[derive(Clone)]
pub struct PluginAccountProvider {
    metadata: AccountProviderInfo,
    guest: account_provider::Guest,
    session: Arc<Session<WasiState>>,
}

impl PluginAccountProvider {
    /// Opens an account-provider session with the caller's WASI capabilities.
    /// Runtime errors or dropped active calls close the session permanently.
    pub async fn open(plugin: &CompiledPlugin, wasi: WasiCtx) -> bottles_plugin_host::Result<Self> {
        let mut linker = Linker::new(plugin.component().engine());
        bottles_plugin_host::add_to_linker(&mut linker)?;
        add_to_linker(&mut linker, |state| state)?;
        let pre = linker.instantiate_pre(plugin.component())?;
        let indices = account_provider::GuestIndices::new(&pre)?;
        let mut invocation = Invocation::new(&pre, WasiState::new(wasi)).await?;
        let guest = indices.load(&mut invocation.store, &invocation.instance)?;
        Ok(Self {
            metadata: AccountProviderInfo {
                id: plugin.info.manifest.id.clone(),
                name: plugin.info.manifest.name.clone().into(),
            },
            guest,
            session: Arc::new(Session::new(invocation)),
        })
    }
}

#[async_trait]
impl AccountProvider for PluginAccountProvider {
    fn metadata(&self) -> AccountProviderInfo {
        self.metadata.clone()
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> Result<LinkedAccount, String> {
        let guest = self.guest.clone();
        self.session
            .call(move |invocation| {
                Box::pin(async move {
                    let interaction = invocation.store.data_mut().table.push(interaction)?;
                    let borrowed = Resource::new_borrow(interaction.rep());
                    let result = guest
                        .call_link_account(&mut invocation.store, borrowed)
                        .await?;
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
