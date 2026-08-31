use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use super::{
    AccountIdentity, AccountLinkInteraction, LinkedAccount, StorefrontAccountProvider,
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
        let linked = self
            .runtime
            .link_account(
                Arc::new(HostAccountLinkInteraction(interaction)),
                cancellation,
            )
            .await?;
        Ok(LinkedAccount {
            identity: AccountIdentity {
                account_id: linked.identity.account_id,
                display_name: linked.identity.display_name,
            },
            credential: linked.credential,
        })
    }
}

struct HostAccountLinkInteraction(Arc<dyn AccountLinkInteraction>);

#[async_trait]
impl bottles_plugin_host::AccountLinkInteraction for HostAccountLinkInteraction {
    async fn request_input(&self, url: String, instructions: String) -> Result<String, String> {
        self.0
            .request_input(
                url.parse()
                    .map_err(|error| format!("invalid interaction URL: {error}"))?,
                instructions,
            )
            .await
    }
}
