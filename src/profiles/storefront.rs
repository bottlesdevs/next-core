//! Storefront capabilities, independent of external plugin package management.

mod plugin;
mod steam;

use crate::{PluginId, PluginKind, Plugins, ProfileError, error::Result};
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

/// Enumerates owned games through a native or external implementation.
#[async_trait]
pub(crate) trait StorefrontLibraryProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn list_games(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> std::result::Result<bottles_plugin_host::ListedGames, String>;
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

/// Resolve library access independently of account linking. Native Steam account
/// discovery does not prevent an external Steam library contribution. Until a
/// library implementation is available, searches report the source as unavailable.
pub(crate) fn library_provider(
    plugins: &Plugins,
    id: &PluginId,
) -> Result<Option<Arc<dyn StorefrontLibraryProvider>>> {
    let plugin = plugins
        .get(id)
        .ok_or_else(|| ProfileError::ProviderNotFound(id.clone()))?;
    Ok(plugin
        .contribution(PluginKind::StorefrontLibraryProvider)
        .map(|plugin| Arc::new(plugin) as Arc<dyn StorefrontLibraryProvider>))
}
