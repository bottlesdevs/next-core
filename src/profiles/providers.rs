//! Account providers and their explicitly registered capabilities.

mod steam;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, collections::HashMap, sync::Arc};

use super::Profiles;

/// Display metadata for an account provider available to [`Profiles`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountProviderInfo {
    /// Stable provider identifier used for linking and persistence.
    pub id: String,
    /// Human-readable provider name.
    pub name: Cow<'static, str>,
}

/// Public identity returned by an account provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountIdentity {
    /// Account identifier local to the provider.
    pub account_id: String,
    /// Human-readable account name.
    pub display_name: String,
}

/// An identified account and its optional private credential.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LinkedAccount {
    /// Public account identity saved in profile state.
    pub identity: AccountIdentity,
    /// Opaque credential saved separately in the platform credential store.
    pub credential: Option<Vec<u8>>,
}

/// Input capability supplied by the caller to one account-link invocation.
#[async_trait]
pub trait AccountLinkInteraction: Send + Sync {
    /// Requests user input for the provider's authentication flow.
    async fn request_input(&self, url: url::Url, instructions: String) -> Result<String, String>;
}

/// Supplies provider metadata and the account-identification flow.
/// Provider identifiers must remain stable while registered.
#[async_trait]
pub trait AccountProvider: Send + Sync {
    /// Returns stable public metadata for this provider.
    fn metadata(&self) -> AccountProviderInfo;
    /// Runs the provider's account-identification or authentication flow.
    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
    ) -> std::result::Result<LinkedAccount, String>;
}

impl Profiles {
    /// Registers a provider, replacing any registration with the same identifier.
    /// Already-running account links retain their original provider.
    pub fn register_provider(&self, provider: Arc<dyn AccountProvider>) {
        let id = provider.metadata().id;
        self.inner.providers.write().unwrap().insert(id, provider);
    }

    /// Removes a provider from future account-link operations.
    /// Existing links and already-running operations keep their original state.
    pub fn remove_provider(&self, provider_id: &str) {
        self.inner.providers.write().unwrap().remove(provider_id);
    }

    /// Lists the currently registered account providers in unspecified order.
    pub fn account_providers(&self) -> Vec<AccountProviderInfo> {
        let providers = self
            .inner
            .providers
            .read()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        providers
            .iter()
            .map(|provider| provider.metadata())
            .collect()
    }
}

pub(super) fn builtins() -> HashMap<String, Arc<dyn AccountProvider>> {
    let steam: Arc<dyn AccountProvider> = Arc::new(steam::Steam);
    HashMap::from([(steam.metadata().id, steam)])
}
