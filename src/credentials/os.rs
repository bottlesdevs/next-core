use keyring::{KeyringEntry, set_global_service_name};
use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

use crate::credentials::{CredentialError, CredentialStore};

const KEYCHAIN_SERVICE: &str = "com.usebottles.Bottles";

pub struct OsCredentialStore;

impl OsCredentialStore {
    pub fn new() -> Self {
        set_global_service_name(KEYCHAIN_SERVICE);
        Self
    }

    fn entry(profile_id: &str, storefront: Storefront) -> Result<KeyringEntry, CredentialError> {
        let account = format!("{profile_id}/{}", storefront.as_str_name(),);
        Ok(KeyringEntry::try_new(account)?)
    }
}

#[async_trait]
impl CredentialStore for OsCredentialStore {
    async fn load(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<Option<String>, CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        match entry.find_secret().await {
            Ok(Some(secret)) => Ok(Some(secret)),
            Ok(None) => Ok(None),
            Err(error) => Err(CredentialError::Store(error.into())),
        }
    }

    async fn save(
        &self,
        profile_id: &str,
        storefront: Storefront,
        secret: &str,
    ) -> Result<(), CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        entry
            .set_secret(secret)
            .await
            .map_err(|error| CredentialError::Store(error.into()))?;

        Ok(())
    }

    async fn delete(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<(), CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        match entry.delete_secret().await {
            Ok(()) => Ok(()),
            Err(error) => Err(CredentialError::Store(error.into())),
        }
    }
}
