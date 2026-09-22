use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    AccountLinkInteraction, LinkedAccount, StorefrontAccountProvider, StorefrontLibraryProvider,
    StorefrontProvider,
};
use bottles_plugin_host::LoadedPlugin;

#[async_trait]
impl StorefrontAccountProvider for LoadedPlugin {
    fn metadata(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: self.info.manifest.id.clone(),
            name: self.info.manifest.name.clone().into(),
        }
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> Result<LinkedAccount, String> {
        bottles_plugin_host::storefront::link_account(&self.component, interaction, cancellation)
            .await
    }
}

#[async_trait]
impl StorefrontLibraryProvider for LoadedPlugin {
    fn name(&self) -> &str {
        &self.info.manifest.name
    }

    async fn list_games(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> Result<bottles_plugin_host::ListedGames, String> {
        bottles_plugin_host::storefront::list_games(&self.component, account_id, credential).await
    }
}
