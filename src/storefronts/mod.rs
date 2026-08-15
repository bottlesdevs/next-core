use next_proto::bottles::{
    common::v1::{Game, LinkedAccount, Storefront},
    store::v1::LoginChallenge,
};
use tonic::{Status, async_trait};

pub mod egs;

#[async_trait]
pub trait StorePlugin: Send + Sync {
    fn storefront(&self) -> Storefront;

    async fn begin_login(&self, profile_id: &str) -> Result<LoginChallenge, Status>;

    async fn complete_login(
        &self,
        profile_id: &str,
        challenge_id: &str,
        user_input: &str,
    ) -> Result<LinkedAccount, Status>;

    async fn refresh_session(&self, profile_id: &str) -> Result<LinkedAccount, Status>;

    async fn revoke_session(&self, profile_id: &str) -> Result<(), Status>;

    async fn games(&self, profile_id: &str) -> Result<Vec<Game>, Status>;
}
