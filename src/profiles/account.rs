use super::{AccountIdentity, StorefrontProvider};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Immutable public metadata for one account link. The live account owns credential synchronization.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StorefrontAccount {
    pub link_id: Uuid,
    pub provider: StorefrontProvider,
    pub identity: AccountIdentity,
}

impl StorefrontAccount {
    pub(super) fn new(
        provider: StorefrontProvider,
        identity: bottles_plugin_host::AccountIdentity,
    ) -> Self {
        Self {
            link_id: Uuid::new_v4(),
            provider,
            identity: AccountIdentity {
                account_id: identity.account_id,
                display_name: identity.display_name,
            },
        }
    }
}

/// Live ownership is separate from public, immutable account snapshots.
pub(super) struct Account {
    pub(super) info: StorefrontAccount,
    pub(super) credential: Mutex<()>,
}

impl Account {
    pub(super) fn new(info: StorefrontAccount) -> Self {
        Self {
            info,
            credential: Mutex::new(()),
        }
    }
}
