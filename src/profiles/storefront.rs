//! Storefront implementations behind one account and library contract.
mod steam;

use crate::{Plugins, ProfileError, error::Result};
use async_trait::async_trait;
use bottles_plugin_host::{LoadedPlugin, PluginInterface};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

pub use bottles_plugin_host::AccountLinkInteraction;
pub(crate) use bottles_plugin_host::{LinkedAccount, OwnedGame};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: String,
    pub name: Cow<'static, str>,
}

pub use bottles_plugin_host::AccountIdentity;

#[async_trait]
pub(super) trait Provider: Send + Sync {
    fn metadata(&self) -> StorefrontProvider;
    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String>;
}

pub(super) fn list(plugins: &Plugins) -> Vec<StorefrontProvider> {
    std::iter::once(steam::metadata())
        .chain(
            plugins
                .list()
                .into_iter()
                .filter(|plugin| plugin.exports(PluginInterface::AccountProvider))
                .map(|plugin| StorefrontProvider {
                    id: plugin_id(&plugin.manifest.id),
                    name: plugin.manifest.name.into(),
                }),
        )
        .collect()
}

pub(super) async fn get(plugins: &Plugins, id: &str) -> Result<Arc<dyn Provider>> {
    if id == steam::metadata().id {
        return Ok(Arc::new(steam::Steam));
    }
    let package_id = id
        .strip_prefix("plugin:")
        .ok_or_else(|| ProfileError::ProviderNotFound(id.into()))?;
    let plugin = plugins.load(package_id).await?;
    if !plugin.info.exports(PluginInterface::AccountProvider) {
        return Err(ProfileError::ProviderNotFound(id.into()).into());
    }
    Ok(Arc::new(plugin))
}

/// Account-only providers have no library to refresh.
pub(super) async fn get_library(plugins: &Plugins, id: &str) -> Result<Option<LoadedPlugin>> {
    if id == steam::metadata().id {
        return Ok(None);
    }
    let package_id = id
        .strip_prefix("plugin:")
        .ok_or_else(|| ProfileError::ProviderNotFound(id.into()))?;
    let plugin = plugins.load(package_id).await?;
    Ok(plugin
        .info
        .exports(PluginInterface::LibraryProvider)
        .then_some(plugin))
}

fn plugin_id(id: &str) -> String {
    format!("plugin:{id}")
}

#[async_trait]
impl Provider for LoadedPlugin {
    fn metadata(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: plugin_id(&self.info.manifest.id),
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
