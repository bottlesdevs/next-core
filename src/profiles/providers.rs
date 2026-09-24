//! Provider discovery and account-link abstraction.
//!
//! The built-in Steam provider is combined with plugins that export the
//! account-provider interface. Persisted profile state receives provider metadata
//! and the public account identity, never the returned credential.
mod steam;

use crate::error::Result;
use async_trait::async_trait;
use bottles_plugin_host::{LoadedPlugin, PluginInterface, Plugins};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

pub use bottles_plugin_host::AccountLinkInteraction;
pub(super) use bottles_plugin_host::LinkedAccount;

/// Display metadata for an account provider available to [`Profiles`].
///
/// [`Profiles`]: super::Profiles
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountProviderInfo {
    /// Stable provider identifier used for linking and persistence.
    pub id: String,
    /// Human-readable provider name.
    pub name: Cow<'static, str>,
}

pub use bottles_plugin_host::AccountIdentity;

/// Supplies provider metadata and the account-identification flow.
#[async_trait]
pub(super) trait AccountProvider: Send + Sync {
    /// Returns stable public metadata for this provider.
    fn metadata(&self) -> AccountProviderInfo;
    /// Runs the provider's account-identification or authentication flow.
    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String>;
}

/// Lists the native provider followed by eligible installed plugins.
pub(super) fn list(plugins: &Plugins) -> Vec<AccountProviderInfo> {
    std::iter::once(steam::metadata())
        .chain(
            plugins
                .list()
                .into_iter()
                .filter(|plugin| {
                    plugin.manifest.id != steam::metadata().id
                        && plugin.exports(PluginInterface::AccountProvider)
                })
                .map(|plugin| AccountProviderInfo {
                    id: plugin.manifest.id,
                    name: plugin.manifest.name.into(),
                }),
        )
        .collect()
}

/// Resolves and loads an account provider by identifier.
///
/// # Errors
///
/// Returns a plugin-host error if a non-native provider cannot be loaded.
pub(super) async fn get(plugins: &Plugins, id: &str) -> Result<Box<dyn AccountProvider>> {
    if id == steam::metadata().id {
        return Ok(Box::new(steam::Steam));
    }
    Ok(Box::new(plugins.load(id).await?))
}

#[async_trait]
impl AccountProvider for LoadedPlugin {
    fn metadata(&self) -> AccountProviderInfo {
        AccountProviderInfo {
            id: self.info.manifest.id.clone(),
            name: self.info.manifest.name.clone().into(),
        }
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String> {
        bottles_plugin_host::storefront::link_account(self, interaction).await
    }
}
