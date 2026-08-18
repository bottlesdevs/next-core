use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use next_proto::bottles::common::v1::Storefront;
use tonic::async_trait;

use crate::credentials::{CredentialError, CredentialStore};

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    credentials: Arc<Mutex<HashMap<(String, Storefront), Vec<u8>>>>,
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
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<Option<Vec<u8>>, CredentialError> {
        Ok(self
            .credentials
            .lock()
            .unwrap()
            .get(&(profile_id.to_owned(), storefront))
            .cloned())
    }

    async fn save(
        &self,
        profile_id: &str,
        storefront: Storefront,
        secret: &[u8],
    ) -> Result<(), CredentialError> {
        self.credentials
            .lock()
            .unwrap()
            .insert((profile_id.to_owned(), storefront), secret.to_vec());

        Ok(())
    }

    async fn delete(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<(), CredentialError> {
        self.credentials
            .lock()
            .unwrap()
            .remove(&(profile_id.to_owned(), storefront));

        Ok(())
    }
}

#[tokio::test]
async fn load_returns_none_for_unknown_entry() {
    let store = MemoryCredentialStore::new();

    let result = store.load("profile-1", Storefront::Steam).await.unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn save_then_load_round_trips() {
    let store = MemoryCredentialStore::new();

    store
        .save("profile-1", Storefront::Steam, b"secret")
        .await
        .unwrap();

    let result = store.load("profile-1", Storefront::Steam).await.unwrap();

    assert_eq!(result, Some(b"secret".to_vec()));
}

#[tokio::test]
async fn save_overwrites_existing_entry() {
    let store = MemoryCredentialStore::new();

    store
        .save("profile-1", Storefront::Steam, b"old")
        .await
        .unwrap();
    store
        .save("profile-1", Storefront::Steam, b"new")
        .await
        .unwrap();

    let result = store.load("profile-1", Storefront::Steam).await.unwrap();

    assert_eq!(result, Some(b"new".to_vec()));
}

#[tokio::test]
async fn delete_removes_entry() {
    let store = MemoryCredentialStore::new();

    store
        .save("profile-1", Storefront::Steam, b"secret")
        .await
        .unwrap();
    store.delete("profile-1", Storefront::Steam).await.unwrap();

    let result = store.load("profile-1", Storefront::Steam).await.unwrap();

    assert_eq!(result, None);
}

#[tokio::test]
async fn delete_on_missing_entry_is_noop() {
    let store = MemoryCredentialStore::new();

    let result = store.delete("profile-1", Storefront::Steam).await;

    assert!(result.is_ok());
}

#[tokio::test]
async fn different_storefronts_are_independent() {
    let store = MemoryCredentialStore::new();

    store
        .save("profile-1", Storefront::Steam, b"steam-secret")
        .await
        .unwrap();
    store
        .save("profile-1", Storefront::Gog, b"gog-secret")
        .await
        .unwrap();

    assert_eq!(
        store.load("profile-1", Storefront::Steam).await.unwrap(),
        Some(b"steam-secret".to_vec())
    );
    assert_eq!(
        store.load("profile-1", Storefront::Gog).await.unwrap(),
        Some(b"gog-secret".to_vec())
    );
}
