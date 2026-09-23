//! Persisted profiles, account links, and selection.

use super::{AccountIdentity, AccountProviderInfo};
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One coherent persisted snapshot of every profile and the selected profile.
///
/// The selected profile is guaranteed to be present in [`profiles`](Self::profiles).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct ProfilesState {
    pub(super) selected: Uuid,
    pub(super) profiles: Vec<Profile>,
}

impl ProfilesState {
    pub(super) fn player() -> Self {
        let profile = Profile {
            id: Uuid::new_v4(),
            name: "Player".into(),
            accounts: Vec::new(),
        };
        Self {
            selected: profile.id,
            profiles: vec![profile],
        }
    }

    pub(crate) fn profile(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Returns every profile in persisted order.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Returns the selected profile from this same snapshot generation.
    pub fn selected(&self) -> &Profile {
        self.profile(self.selected)
            .expect("selected profile was validated")
    }
}

/// An immutable application-profile snapshot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Profile {
    pub(super) id: Uuid,
    pub(super) name: String,
    #[serde(default)]
    pub(super) accounts: Vec<AccountLink>,
}

impl Profile {
    /// Returns the profile's stable identity.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the profile's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns public metadata for the storefront accounts linked to this profile.
    pub fn accounts(&self) -> &[AccountLink] {
        &self.accounts
    }
}

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
