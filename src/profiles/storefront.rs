//! Native and plugin account providers for profiles.
mod steam;

use crate::{ProfileError, error::Result};
use bottles_plugin_host::{LoadedPlugin, PluginInterface, Plugins};
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

pub(crate) enum Provider {
    Steam,
    Plugin(LoadedPlugin),
}

impl Provider {
    pub(crate) fn metadata(&self) -> StorefrontProvider {
        match self {
            Self::Steam => steam::metadata(),
            Self::Plugin(plugin) => StorefrontProvider {
                id: plugin.info.manifest.id.clone(),
                name: plugin.info.manifest.name.clone().into(),
            },
        }
    }

    pub(crate) async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String> {
        match self {
            Self::Steam => steam::link_account().await,
            Self::Plugin(plugin) => {
                bottles_plugin_host::storefront::link_account(plugin, interaction).await
            }
        }
    }

    pub(crate) fn library(&self) -> Option<&LoadedPlugin> {
        match self {
            Self::Plugin(plugin) if plugin.info.exports(PluginInterface::LibraryProvider) => {
                Some(plugin)
            }
            _ => None,
        }
    }
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

pub(crate) async fn get(plugins: &Plugins, id: &str) -> Result<Provider> {
    if id == steam::metadata().id {
        return Ok(Provider::Steam);
    }
    let plugin = plugins.load(id).await?;
    if !plugin.info.exports(PluginInterface::AccountProvider) {
        return Err(ProfileError::ProviderNotFound(id.into()).into());
    }
    Ok(Provider::Plugin(plugin))
}
