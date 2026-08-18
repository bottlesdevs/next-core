use keyring::Entry;
use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

use crate::credentials::{CredentialError, CredentialStore};

const KEYCHAIN_SERVICE: &str = "com.usebottles.Bottles";

pub struct OsCredentialStore;

impl OsCredentialStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(profile_id: &str, storefront: Storefront) -> Result<Entry, CredentialError> {
        let account = format!("profile/{profile_id}/{}", storefront.as_str_name(),);

        Ok(Entry::new(KEYCHAIN_SERVICE, &account)?)
    }
}

#[async_trait]
impl CredentialStore for OsCredentialStore {
    async fn load(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        match entry.get_secret() {
            Ok(secret) => Ok(Some(secret)),

            Err(keyring::Error::NoEntry) => Ok(None),

            Err(error) => Err(CredentialError::Store(error)),
        }
    }

    async fn save(
        &self,
        profile_id: &str,
        storefront: Storefront,
        secret: &[u8],
    ) -> Result<(), CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        entry.set_secret(secret)?;

        Ok(())
    }

    async fn delete(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<(), CredentialError> {
        let entry = Self::entry(profile_id, storefront)?;

        match entry.delete_credential() {
            Ok(()) => Ok(()),

            Err(keyring::Error::NoEntry) => Ok(()),

            Err(error) => Err(CredentialError::Store(error)),
        }
    }
}
