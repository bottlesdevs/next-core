use async_trait::async_trait;
use keyring::Entry;
use uuid::{NonNilUuid, Uuid};

use crate::credentials::{CredentialError, CredentialStore};

const KEYCHAIN_SERVICE: &str = "com.usebottles.bottles-next";

pub(crate) struct KeyringCredentialStore;

impl KeyringCredentialStore {
    pub(crate) fn new() -> Self {
        Self
    }

    fn entry(provider_id: NonNilUuid, profile_id: Uuid) -> Result<Entry, CredentialError> {
        let account = format!("providers/{provider_id}/profiles/{profile_id}");
        Ok(Entry::new(KEYCHAIN_SERVICE, &account)?)
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn load(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        blocking::unblock(
            move || match Self::entry(provider_id, profile_id)?.get_secret() {
                Ok(secret) => Ok(Some(secret)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(error.into()),
            },
        )
        .await
    }

    async fn save(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
        secret: &[u8],
    ) -> Result<(), CredentialError> {
        let secret = secret.to_vec();
        blocking::unblock(move || {
            Self::entry(provider_id, profile_id)?.set_secret(&secret)?;
            Ok(())
        })
        .await
    }

    async fn delete(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<(), CredentialError> {
        blocking::unblock(
            move || match Self::entry(provider_id, profile_id)?.delete_credential() {
                Ok(()) => Ok(()),
                Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.into()),
            },
        )
        .await
    }
}
