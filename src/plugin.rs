//! Shared plugin bindings, interface identifiers, and provider loading.

use std::sync::Arc;

use bottles_plugin_host::{Plugin, PluginInfo, Plugins, WasiState};
use wasmtime::{
    Store,
    component::{HasSelf, Instance, Linker},
};

include!(concat!(env!("OUT_DIR"), "/plugin_interfaces.rs"));

/// Native account interaction stored in a plugin's resource table.
pub type Interaction = Arc<dyn crate::AccountLinkInteraction>;

wasmtime::component::bindgen!({
    path: "../next-plugin-api/wit",
    world: "core",
    imports: { default: trappable },
    with: {
        "bottles:plugin/account-link.interaction": Interaction,
    },
});

fn register_imports(linker: &mut Linker<WasiState>) -> wasmtime::Result<()> {
    Core::add_to_linker::<_, HasSelf<WasiState>>(linker, |state| state)
}

/// Opens a provider using core's default state and complete host import environment.
pub(crate) async fn load_plugin<Bindings: Send>(
    plugins: &Plugins,
    info: &PluginInfo,
    load_exports: impl FnOnce(&mut Store<WasiState>, &Instance) -> wasmtime::Result<Bindings> + Send,
) -> bottles_plugin_host::Result<Plugin<Bindings>> {
    plugins.load(info, register_imports, load_exports).await
}
