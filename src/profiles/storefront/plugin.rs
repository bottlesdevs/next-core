use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    AccountLinkInteraction, LinkedAccount, StorefrontAccountProvider, StorefrontLibraryProvider,
    StorefrontProvider,
};
use crate::plugins::Plugin;

#[async_trait]
impl StorefrontAccountProvider for Plugin {
    fn metadata(&self) -> StorefrontProvider {
        StorefrontProvider {
            id: self.manifest.id.clone(),
            name: self.manifest.name.clone().into(),
        }
    }

    async fn link_account(
        &self,
        interaction: Arc<dyn AccountLinkInteraction>,
        cancellation: &CancellationToken,
    ) -> Result<LinkedAccount, String> {
        self.runtime.link_account(interaction, cancellation).await
    }
}

#[async_trait]
impl StorefrontLibraryProvider for Plugin {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    async fn list_games(
        &self,
        account_id: &str,
        credential: Option<&[u8]>,
    ) -> Result<bottles_plugin_host::ListedGames, String> {
        self.runtime.list_games(account_id, credential).await
    }
}
