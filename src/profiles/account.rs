use super::{AccountIdentity, AccountProviderInfo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Persisted public metadata for one account link.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountLink {
    pub link_id: Uuid,
    pub provider: AccountProviderInfo,
    pub identity: AccountIdentity,
}

impl AccountLink {
    pub(super) fn new(provider: AccountProviderInfo, identity: AccountIdentity) -> Self {
        Self {
            link_id: Uuid::new_v4(),
            provider,
            identity,
        }
    }
}
