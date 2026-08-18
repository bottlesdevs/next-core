use next_proto::bottles::common::v1::{LinkedAccount, Storefront};
use next_proto::bottles::profiles::v1::UserProfile;

use crate::error::Result;
use crate::profile::ProfileManager;

mod error;
pub use error::AccountError;

pub struct AccountManager {
    profiles: ProfileManager,
}

impl AccountManager {
    pub fn new(profiles: ProfileManager) -> Self {
        Self { profiles }
    }

    pub async fn link_profile(
        &self,
        profile_id: &str,
        account: LinkedAccount,
    ) -> Result<UserProfile> {
        let mut profile = self.profiles.get(&profile_id).await?;
        profile
            .accounts
            .retain(|existing| existing.storefront != account.storefront() as i32);
        profile.accounts.push(account);
        self.profiles.update(profile.clone()).await?;
        Ok(profile)
    }

    pub async fn unlink_profile(
        &self,
        profile_id: &str,
        storefront: Storefront,
    ) -> Result<UserProfile> {
        let mut profile = self.profiles.get(profile_id).await?;
        profile
            .accounts
            .retain(|account| account.storefront != storefront as i32);
        self.profiles.update(profile.clone()).await?;
        Ok(profile)
    }
}
