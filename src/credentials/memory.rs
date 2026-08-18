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
