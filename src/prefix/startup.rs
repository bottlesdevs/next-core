//! Effects applied each time a prefix starts `WineBridge`.
//!
//! The runner, components in slot order, and dependencies in installation
//! order contribute in turn, so a later addon overrides an earlier one. The
//! user's environment variables are applied last. DLL overrides are passed
//! through `WINEDLLOVERRIDES`. Registry values are written through
//! `WineBridge` and are never deleted, so a value stays after its effect stops
//! applying.

use std::collections::BTreeMap;

use strum::IntoEnumIterator;

use super::PrefixState;
use crate::{
    EnvVars, InstalledAddon, Slot, addons::Effect, error::Result, proto::DllOverrideMode,
    winebridge::WineBridgeClient,
};

const DLL_OVERRIDES: &str = "WINEDLLOVERRIDES";

impl PrefixState {
    /// Returns the effects of the selected addons in the order they apply,
    /// with templates rendered.
    fn effects(&self) -> impl Iterator<Item = Effect> + '_ {
        effects(&self.runner)
            .chain(
                Slot::iter()
                    .filter_map(|slot| self.component(slot))
                    .flat_map(effects),
            )
            .chain(self.dependencies.iter().flat_map(effects))
    }

    /// Returns the environment of the started `WineBridge`.
    ///
    /// Addon DLL overrides are folded into `WINEDLLOVERRIDES`. A value the
    /// user set wins for the DLLs it names, and its entries without a mode are
    /// kept as written.
    pub(super) fn env(&self) -> EnvVars {
        let mut env = EnvVars::default();
        let mut dll_overrides = BTreeMap::new();
        for effect in self.effects() {
            match effect {
                Effect::Environment { name, value, .. } => {
                    env.insert(name, value);
                }
                Effect::DllOverrides { dlls, mode, .. } => {
                    if let Some(mode) = wine_mode(mode) {
                        for dll in dlls {
                            dll_overrides.insert(dll, mode.to_owned());
                        }
                    }
                }
                Effect::RegistryValue { .. } => {}
            }
        }
        env.extend(self.env_vars.clone());
        let mut unparsed = Vec::new();
        if let Some(value) = env.get(DLL_OVERRIDES) {
            for entry in value.split(';').filter(|entry| !entry.is_empty()) {
                match entry.split_once('=') {
                    Some((dlls, mode)) => {
                        for dll in dlls.split(',') {
                            dll_overrides.insert(dll.to_owned(), mode.to_owned());
                        }
                    }
                    None => unparsed.push(entry.to_owned()),
                }
            }
        }
        if !dll_overrides.is_empty() {
            let entries = dll_overrides
                .iter()
                .map(|(dll, mode)| format!("{dll}={mode}"))
                .chain(unparsed);
            env.insert(DLL_OVERRIDES.into(), entries.collect::<Vec<_>>().join(";"));
        }
        env
    }

    /// Writes the registry values of the selected addons through `bridge`.
    ///
    /// # Errors
    ///
    /// Returns an error if writing a value fails.
    pub(super) async fn write_registry(&self, bridge: &WineBridgeClient) -> Result<()> {
        for effect in self.effects() {
            if let Effect::RegistryValue {
                hive,
                key,
                name,
                value,
                ..
            } = effect
            {
                bridge.set_registry_value(hive, key, name, value).await?;
            }
        }
        Ok(())
    }
}

/// Returns the effects of `addon` with templates rendered.
fn effects<R>(addon: &InstalledAddon<R>) -> impl Iterator<Item = Effect> + '_ {
    addon.addon.settings().effects(addon.settings())
}

/// Returns the `WINEDLLOVERRIDES` spelling of `mode`.
fn wine_mode(mode: DllOverrideMode) -> Option<&'static str> {
    match mode {
        DllOverrideMode::Unspecified => None,
        DllOverrideMode::NativeBuiltin => Some("n,b"),
        DllOverrideMode::BuiltinNative => Some("b,n"),
        DllOverrideMode::Native => Some("n"),
        DllOverrideMode::Builtin => Some("b"),
        DllOverrideMode::Disabled => Some(""),
    }
}
