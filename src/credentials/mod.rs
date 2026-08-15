pub mod memory;
pub mod os;

use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential store error: {0}")]
    Store(#[from] keyring::Error),

    #[error("credential not found")]
    NotFound,

    #[error("invalid credential data: {0}")]
    InvalidData(String),
}

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
