use next_proto::bottles::{
    common::v1::{Game, LinkedAccount, Storefront},
    store::v1::LoginChallenge,
};
use tonic::async_trait;

pub use crate::plugins::error::PluginError;

pub mod egs;
pub mod error;

#[async_trait]
pub trait StorePlugin: Send + Sync {
    fn storefront(&self) -> Storefront;

    async fn begin_login(&self, profile_id: &str) -> Result<LoginChallenge, PluginError>;

    async fn complete_login(
        &self,
        profile_id: &str,
        challenge_id: &str,
        user_input: &str,
    ) -> Result<LinkedAccount, PluginError>;

    async fn refresh_session(&self, profile_id: &str) -> Result<LinkedAccount, PluginError>;

    async fn revoke_session(&self, profile_id: &str) -> Result<(), PluginError>;

    async fn games(&self, profile_id: &str) -> Result<Vec<Game>, PluginError>;
}
