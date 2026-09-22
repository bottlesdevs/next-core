//! Storefront implementations behind one account and library contract.
mod steam;

use crate::{Plugins, ProfileError, error::Result};
use async_trait::async_trait;
use bottles_plugin_host::{LoadedPlugin, PluginInterface};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

pub use bottles_plugin_host::AccountLinkInteraction;
pub(crate) use bottles_plugin_host::{Authentication, LinkedAccount, OwnedGame};

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
    async fn authenticate(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> std::result::Result<Authentication, String>;
    async fn list_games(
        &self,
        account_id: &str,
        access: &[u8],
    ) -> std::result::Result<Vec<OwnedGame>, String>;
}

pub(super) fn list(plugins: &Plugins) -> Vec<StorefrontProvider> {
    std::iter::once(steam::metadata())
        .chain(
            plugins
                .list()
                .into_iter()
                .filter(|plugin| plugin.exports(PluginInterface::StorefrontProvider))
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
    if !plugin.info.exports(PluginInterface::StorefrontProvider) {
        return Err(ProfileError::ProviderNotFound(id.into()).into());
    }
    Ok(Arc::new(plugin))
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

    async fn authenticate(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> std::result::Result<Authentication, String> {
        bottles_plugin_host::storefront::authenticate(self, account_id, credential).await
    }

    async fn list_games(
        &self,
        account_id: &str,
        access: &[u8],
    ) -> std::result::Result<Vec<OwnedGame>, String> {
        bottles_plugin_host::storefront::list_games(self, account_id, access).await
    }
}
