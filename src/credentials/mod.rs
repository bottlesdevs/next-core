mod error;
mod os;
#[cfg(test)]
pub(crate) mod tests;

use async_trait::async_trait;
use uuid::{NonNilUuid, Uuid};

pub(crate) use error::CredentialError;
pub(crate) use os::KeyringCredentialStore;

#[async_trait]
pub(crate) trait CredentialStore: Send + Sync {
    async fn load(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<Option<Vec<u8>>, CredentialError>;

    async fn save(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
        secret: &[u8],
    ) -> Result<(), CredentialError>;

    async fn delete(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<(), CredentialError>;
}
