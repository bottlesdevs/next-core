//! Storefront capabilities, independent of external plugin package management.

mod plugin;
mod steam;

use crate::{PluginId, PluginKind, Plugins, ProfileError, error::Result, plugins::Plugin};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Static identity of one available storefront account provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: PluginId,
    pub name: Cow<'static, str>,
}

/// Public account metadata returned by a storefront provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    pub account_id: String,
    pub display_name: String,
}

/// Account metadata and credential produced by a successful provider link.
pub(crate) struct LinkedAccount {
    pub identity: AccountIdentity,
    pub credential: Option<Vec<u8>>,
}

/// Supplies provider-directed interaction while an account is being linked.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: url::Url,
        instructions: String,
    ) -> std::result::Result<String, String>;
}

/// Links one storefront account through a native or Wasm provider.
#[async_trait]
pub(crate) trait StorefrontAccountProvider: Send + Sync {
    fn metadata(&self) -> StorefrontProvider;

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> std::result::Result<LinkedAccount, String>;
}

pub(crate) fn account_provider(
    plugins: &Plugins,
    id: &PluginId,
) -> Result<Arc<dyn StorefrontAccountProvider>> {
    if id == &steam::PROVIDER_ID {
        return Ok(Arc::new(steam::SteamIntegration));
    }
    plugins
        .contribution(id, PluginKind::StorefrontAccountProvider)
        .map(|plugin| Arc::new(plugin) as Arc<dyn StorefrontAccountProvider>)
        .ok_or_else(|| ProfileError::ProviderNotFound(id.clone()).into())
}

pub(crate) fn account_providers(plugins: &Plugins) -> Vec<StorefrontProvider> {
    std::iter::once(steam::METADATA)
        .chain(
            plugins
                .contributions(PluginKind::StorefrontAccountProvider)
                .into_iter()
                .filter(|plugin| plugin.manifest.id != steam::PROVIDER_ID)
                .map(|plugin| plugin.metadata()),
        )
        .collect()
}

pub(crate) fn library_provider(plugins: &Plugins, id: &PluginId) -> Option<Plugin> {
    // Built-in identity takes precedence even when an external package uses this ID.
    if id == &steam::PROVIDER_ID {
        return None;
    }
    plugins.contribution(id, PluginKind::StorefrontLibraryProvider)
}
