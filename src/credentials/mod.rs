use keyring::Entry;
use uuid::Uuid;

use crate::PluginId;

#[cfg(test)]
use std::sync::{Arc, OnceLock};

const SERVICE: &str = "com.usebottles.bottles-next";

fn account(provider_id: &PluginId, profile_id: Uuid) -> String {
    format!("providers/{provider_id}/profiles/{profile_id}")
}

fn entry(account: &str) -> keyring::Result<Entry> {
    #[cfg(test)]
    if let Some(store) = TEST_STORE.get() {
        return Ok(Entry {
            inner: store.build(SERVICE, account, None)?,
        });
    }
    Entry::new(SERVICE, account)
}

#[cfg(test)]
static TEST_STORE: OnceLock<Arc<keyring_core::CredentialStore>> = OnceLock::new();

#[cfg(test)]
pub(crate) fn use_test_store() {
    TEST_STORE.get_or_init(|| keyring_core::mock::Store::new().unwrap());
}

fn load_entry(entry: &Entry) -> keyring::Result<Option<Vec<u8>>> {
    match entry.get_secret() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) async fn load(
    provider_id: &PluginId,
    profile_id: Uuid,
) -> keyring::Result<Option<Vec<u8>>> {
    let account = account(provider_id, profile_id);
    blocking::unblock(move || load_entry(&entry(&account)?)).await
}

fn save_entry(entry: &Entry, secret: &[u8]) -> keyring::Result<()> {
    entry.set_secret(secret)
}

fn delete_entry(entry: &Entry) -> keyring::Result<()> {
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) async fn save(
    provider_id: &PluginId,
    profile_id: Uuid,
    secret: &[u8],
) -> keyring::Result<()> {
    let account = account(provider_id, profile_id);
    let secret = secret.to_vec();
    blocking::unblock(move || save_entry(&entry(&account)?, &secret)).await
}

pub(crate) async fn delete(provider_id: &PluginId, profile_id: Uuid) -> keyring::Result<()> {
    let account = account(provider_id, profile_id);
    blocking::unblock(move || delete_entry(&entry(&account)?)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_operations_are_idempotent_and_isolated() {
        use_test_store();
        futures_lite::future::block_on(async {
            let provider = PluginId::new("provider");
            let profile = Uuid::new_v4();
            let other_provider = PluginId::new("other-provider");
            let other_profile = Uuid::new_v4();

            assert_eq!(load(&provider, profile).await.unwrap(), None);
            save(&provider, profile, b"old").await.unwrap();
            save(&provider, profile, b"new").await.unwrap();
            assert_eq!(
                load(&provider, profile).await.unwrap(),
                Some(b"new".to_vec())
            );
            assert_eq!(load(&other_provider, profile).await.unwrap(), None);
            assert_eq!(load(&provider, other_profile).await.unwrap(), None);
            delete(&provider, profile).await.unwrap();
            delete(&provider, profile).await.unwrap();
            assert_eq!(load(&provider, profile).await.unwrap(), None);
        });
    }
}
