use super::{AccountIdentity, StorefrontProvider};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted public metadata for one account link.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontAccount {
    pub link_id: Uuid,
    pub provider: StorefrontProvider,
    pub identity: AccountIdentity,
}

impl StorefrontAccount {
    pub(super) fn new(provider: StorefrontProvider, identity: AccountIdentity) -> Self {
        Self {
            link_id: Uuid::new_v4(),
            provider,
            identity,
        }
    }
}
