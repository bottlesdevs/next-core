//! Serializable snapshots of profiles, selection, and public account data.

use super::{AccountIdentity, AccountProviderInfo};
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A coherent snapshot of every profile and the current selection.
///
/// Snapshots returned by [`Profiles::state`](super::Profiles::state) always
/// select an entry present in [`profiles`](Self::profiles).
///
/// # Examples
///
/// ```
/// # fn inspect(state: &bottles_core::ProfilesState) {
/// assert!(state.profiles().iter().any(|profile| {
///     profile.id() == state.selected().id()
/// }));
/// # }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, Config)]
#[config(version = 1)]
pub struct ProfilesState {
    pub(super) selected: Uuid,
    pub(super) profiles: Vec<Profile>,
}

impl ProfilesState {
    /// Creates the initial state containing one selected `Player` profile.
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

    /// Finds a profile by UUID within this snapshot.
    pub(crate) fn profile(&self, id: Uuid) -> Option<&Profile> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    /// Returns all profiles in persisted order.
    pub fn profiles(&self) -> &[Profile] {
        &self.profiles
    }

    /// Returns the selected profile from this snapshot.
    ///
    /// # Panics
    ///
    /// Panics if a caller independently deserialized an invalid snapshot whose
    /// selected UUID is absent from its profile list. Snapshots obtained from
    /// [`Profiles`](super::Profiles) are validated before publication.
    pub fn selected(&self) -> &Profile {
        self.profile(self.selected)
            .expect("selected profile was validated")
    }
}

/// Public data for one application profile.
///
/// Values returned from [`Profiles`](super::Profiles) are detached snapshots;
/// use the manager's mutation methods to make persistent changes.
///
/// # Examples
///
/// ```
/// # fn display(profile: &bottles_core::Profile) {
/// println!("{} ({})", profile.name(), profile.id());
/// # }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Profile {
    pub(super) id: Uuid,
    pub(super) name: String,
    #[serde(default)]
    pub(super) accounts: Vec<AccountLink>,
}

impl Profile {
    /// Returns the stable UUID assigned when the profile was created.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Returns the profile's display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the public account links associated with the profile.
    pub fn accounts(&self) -> &[AccountLink] {
        &self.accounts
    }
}

/// Public metadata for one provider account linked to a profile.
///
/// Credentials are not stored in this value; they live in the platform
/// credential store under [`link_id`](Self::link_id).
///
/// # Examples
///
/// ```
/// # fn provider_id(link: &bottles_core::AccountLink) -> &str {
/// &link.provider.id
/// # }
/// ```
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AccountLink {
    /// Unique identifier for this link and its credential-store entry.
    pub link_id: Uuid,
    /// Provider that created the link.
    pub provider: AccountProviderInfo,
    /// Provider-defined public account identity.
    pub identity: AccountIdentity,
}

impl AccountLink {
    /// Creates public link metadata with a newly generated UUID.
    pub(super) fn new(provider: AccountProviderInfo, identity: AccountIdentity) -> Self {
        Self {
            link_id: Uuid::new_v4(),
            provider,
            identity,
        }
    }
}
