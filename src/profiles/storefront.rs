//! Storefront implementations behind one account and library contract.
mod steam;
pub(super) mod wasm;
pub use wasm::add_plugin_imports;
pub(crate) use wasm::{Authentication, LinkedAccount, OwnedGame};

use crate::{Plugins, ProfileError, error::Result};
use async_trait::async_trait;
use bottles_plugin_host::{LoadedPlugin, PluginInterface};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, sync::Arc};
use tokio_util::sync::CancellationToken;

/// Application-owned input interaction passed explicitly to a storefront invocation.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    async fn request_input(
        &self,
        url: url::Url,
        instructions: String,
    ) -> std::result::Result<String, String>;
}

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
        cancellation: &CancellationToken,
    ) -> std::result::Result<LinkedAccount, String> {
        wasm::link_account(&self.component, interaction, cancellation).await
    }

    async fn authenticate(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> std::result::Result<Authentication, String> {
        wasm::authenticate(&self.component, account_id, credential).await
    }

    async fn list_games(
        &self,
        account_id: &str,
        access: &[u8],
        cancellation: &CancellationToken,
    ) -> std::result::Result<Vec<OwnedGame>, String> {
        wasm::list_games(&self.component, account_id, access, cancellation).await
    }
}
