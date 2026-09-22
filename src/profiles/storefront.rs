//! Storefront implementations behind one account and library contract.
mod steam;

use crate::{Plugins, ProfileError, error::Result};
use async_trait::async_trait;
use bottles_plugin_host::{Authentication, LoadedPlugin, OwnedGame, PluginInterface};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

pub use bottles_plugin_host::AccountLinkInteraction;
pub(crate) use bottles_plugin_host::LinkedAccount;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontProvider {
    pub id: String,
    pub name: Cow<'static, str>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    pub account_id: String,
    pub display_name: String,
}

#[async_trait]
pub(super) trait Provider: Send + Sync {
    fn metadata(&self) -> StorefrontProvider;
    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
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
        cancellation: &CancellationToken,
    ) -> std::result::Result<Vec<OwnedGame>, String>;
}

/// Provider construction is confined to the storefront integration.
pub(super) struct Storefronts {
    plugins: Arc<Plugins>,
    native: HashMap<String, Arc<dyn Provider>>,
}

impl Storefronts {
    pub(super) fn new(plugins: Arc<Plugins>) -> Self {
        let steam: Arc<dyn Provider> = Arc::new(steam::Steam);
        Self {
            plugins,
            native: [(steam.metadata().id, steam)].into(),
        }
    }

    pub(super) fn list(&self) -> Vec<StorefrontProvider> {
        self.native
            .values()
            .map(|p| p.metadata())
            .chain(
                self.plugins
                    .list()
                    .into_iter()
                    .filter(|p| p.exports(PluginInterface::StorefrontProvider))
                    .map(|p| StorefrontProvider {
                        id: plugin_id(&p.manifest.id),
                        name: p.manifest.name.into(),
                    }),
            )
            .collect()
    }

    pub(super) async fn get(&self, id: &str) -> Result<Arc<dyn Provider>> {
        if let Some(provider) = self.native.get(id) {
            return Ok(provider.clone());
        }
        let package_id = id
            .strip_prefix("plugin:")
            .ok_or_else(|| ProfileError::ProviderNotFound(id.into()))?;
        let plugin = self.plugins.load(package_id).await?;
        if !plugin.info.exports(PluginInterface::StorefrontProvider) {
            return Err(ProfileError::ProviderNotFound(id.into()).into());
        }
        Ok(Arc::new(plugin))
    }
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
        cancellation: &CancellationToken,
    ) -> std::result::Result<LinkedAccount, String> {
        bottles_plugin_host::storefront::link_account(&self.component, interaction, cancellation)
            .await
    }

    async fn authenticate(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> std::result::Result<Authentication, String> {
        bottles_plugin_host::storefront::authenticate(&self.component, account_id, credential).await
    }

    async fn list_games(
        &self,
        account_id: &str,
        access: &[u8],
        cancellation: &CancellationToken,
    ) -> std::result::Result<Vec<OwnedGame>, String> {
        bottles_plugin_host::storefront::list_games(
            &self.component,
            account_id,
            access,
            cancellation,
        )
        .await
    }
}
