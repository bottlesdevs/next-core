mod error;
pub mod memory;
pub mod os;

use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

pub use crate::credentials::error::CredentialError;

#[async_trait]
pub trait CredentialStore {
    async fn load(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<Option<Vec<u8>>, CredentialError>;

    async fn save(
        &self,
        profile_id: &str,
        storefront: Storefront,
        credentials: &[u8],
    ) -> Result<(), CredentialError>;

    async fn delete(&self, profile_id: &str, storefront: Storefront)
    -> Result<(), CredentialError>;
}
