//! Native and plugin account providers for profiles.
mod steam;

use crate::{ProfileError, error::Result};
use async_trait::async_trait;
use bottles_plugin_host::{Authentication, LoadedPlugin, OwnedGame, PluginInterface, Plugins};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};

pub use bottles_plugin_host::AccountLinkInteraction;
pub(super) use bottles_plugin_host::LinkedAccount;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: String,
    pub name: Cow<'static, str>,
}

pub use bottles_plugin_host::AccountIdentity;

#[async_trait]
pub(super) trait AccountProvider: Send + Sync {
    fn metadata(&self) -> StorefrontProvider;
    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String>;
}

/// Authentication and enumeration, independent of catalog storage and account linking.
#[async_trait]
pub(super) trait LibraryProvider: Send + Sync {
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
                .filter(|plugin| {
                    plugin.manifest.id != steam::metadata().id
                        && plugin.exports(PluginInterface::AccountProvider)
                })
                .map(|plugin| StorefrontProvider {
                    id: plugin.manifest.id,
                    name: plugin.manifest.name.into(),
                }),
        )
        .collect()
}

pub(super) async fn get(plugins: &Plugins, id: &str) -> Result<Box<dyn AccountProvider>> {
    if id == steam::metadata().id {
        return Ok(Box::new(steam::Steam));
    }
    let plugin = plugins.load(id).await?;
    if !plugin.info.exports(PluginInterface::AccountProvider) {
        return Err(ProfileError::ProviderNotFound(id.into()).into());
    }
    Ok(Box::new(plugin))
}

#[async_trait]
impl AccountProvider for LoadedPlugin {
    fn metadata(&self) -> StorefrontProvider {
        StorefrontProvider {
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

#[async_trait]
impl LibraryProvider for LoadedPlugin {
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
