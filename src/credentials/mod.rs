mod error;
pub mod os;
#[cfg(test)]
mod tests;

use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

pub use crate::credentials::error::CredentialError;

#[async_trait]
pub trait CredentialStore {
    async fn load(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<Option<String>, CredentialError>;

    async fn save(
        &self,
        profile_id: &str,
        storefront: Storefront,
        credentials: &str,
    ) -> Result<(), CredentialError>;

    async fn delete(&self, profile_id: &str, storefront: Storefront)
    -> Result<(), CredentialError>;
}
