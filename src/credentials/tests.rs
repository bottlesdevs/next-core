use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use uuid::{NonNilUuid, Uuid};

use crate::credentials::{CredentialError, CredentialStore};

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    credentials: Arc<Mutex<HashMap<(NonNilUuid, Uuid), Vec<u8>>>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl CredentialStore for MemoryCredentialStore {
    async fn load(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        Ok(self
            .credentials
            .lock()
            .unwrap()
            .get(&(provider_id, profile_id))
            .cloned())
    }

    async fn save(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
        secret: &[u8],
    ) -> Result<(), CredentialError> {
        self.credentials
            .lock()
            .unwrap()
            .insert((provider_id, profile_id), secret.to_vec());

        Ok(())
    }

    async fn delete(
        &self,
        provider_id: NonNilUuid,
        profile_id: Uuid,
    ) -> Result<(), CredentialError> {
        self.credentials
            .lock()
            .unwrap()
            .remove(&(provider_id, profile_id));

        Ok(())
    }
}

#[tokio::test]
async fn load_returns_none_for_unknown_entry() {
    let store = MemoryCredentialStore::new();
    let (provider, profile) = ids();

    let result = store.load(provider, profile).await.unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn save_then_load_round_trips() {
    let store = MemoryCredentialStore::new();
    let (provider, profile) = ids();

    store.save(provider, profile, b"secret").await.unwrap();

    let result = store.load(provider, profile).await.unwrap();

    assert_eq!(result, Some(b"secret".to_vec()));
}

#[tokio::test]
async fn save_overwrites_existing_entry() {
    let store = MemoryCredentialStore::new();
    let (provider, profile) = ids();

    store.save(provider, profile, b"old").await.unwrap();
    store.save(provider, profile, b"new").await.unwrap();

    let result = store.load(provider, profile).await.unwrap();

    assert_eq!(result, Some(b"new".to_vec()));
}

#[tokio::test]
async fn delete_removes_entry() {
    let store = MemoryCredentialStore::new();
    let (provider, profile) = ids();

    store.save(provider, profile, b"secret").await.unwrap();
    store.delete(provider, profile).await.unwrap();

    let result = store.load(provider, profile).await.unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn delete_on_missing_entry_is_noop() {
    let store = MemoryCredentialStore::new();
    let (provider, profile) = ids();

    let result = store.delete(provider, profile).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn provider_and_profile_namespaces_are_independent() {
    let store = MemoryCredentialStore::new();
    let profile_a = Uuid::new_v4();
    let profile_b = Uuid::new_v4();
    let provider_a = NonNilUuid::new(Uuid::new_v4()).unwrap();
    let provider_b = NonNilUuid::new(Uuid::new_v4()).unwrap();

    store.save(provider_a, profile_a, b"a-a").await.unwrap();
    store.save(provider_b, profile_a, b"b-a").await.unwrap();
    store.save(provider_a, profile_b, b"a-b").await.unwrap();

    assert_eq!(
        store.load(provider_a, profile_a).await.unwrap(),
        Some(b"a-a".to_vec())
    );
    assert_eq!(
        store.load(provider_b, profile_a).await.unwrap(),
        Some(b"b-a".to_vec())
    );
    assert_eq!(
        store.load(provider_a, profile_b).await.unwrap(),
        Some(b"a-b".to_vec())
    );
}

fn ids() -> (NonNilUuid, Uuid) {
    (NonNilUuid::new(Uuid::new_v4()).unwrap(), Uuid::new_v4())
}
