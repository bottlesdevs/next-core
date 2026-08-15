use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use egs_api::EpicGames;
use next_proto::bottles::{
    common::v1::{AuthState, Game, LinkedAccount, Storefront},
    store::v1::{BrowserRedirectChallenge, LoginChallenge, LoginInputKind, login_challenge::Kind},
};
use tokio::sync::RwLock;
use tonic::{Status, async_trait};
use uuid::Uuid;

use crate::{credentials::CredentialStore, storefronts::StorePlugin};

struct PendingChallenge {
    created_at: Instant,
}

pub struct EpicGamesService<C: CredentialStore> {
    challenges: Arc<RwLock<HashMap<String, PendingChallenge>>>,
    credentials: Arc<C>,
}

impl<C> EpicGamesService<C>
where
    C: CredentialStore,
{
    pub fn new(credentials: Arc<C>) -> Self {
        Self {
            challenges: Arc::new(RwLock::new(HashMap::new())),
            credentials,
        }
    }
}

#[async_trait]
impl<C: CredentialStore + Send + Sync + 'static> StorePlugin for EpicGamesService<C> {
    fn storefront(&self) -> Storefront {
        Storefront::EpicGames
    }

    async fn begin_login(&self, _profile_id: &str) -> Result<LoginChallenge, Status> {
        let challenge_id = Uuid::new_v4().to_string();

        self.challenges.write().await.insert(
            challenge_id.clone(),
            PendingChallenge {
                created_at: Instant::now(),
            },
        );

        let kind = Kind::BrowserRedirect(BrowserRedirectChallenge {
            url: "https://www.epicgames.com/id/login?redirectUrl=https%3A%2F%2Fwww.epicgames.com%2Fid%2Fapi%2Fredirect%3FclientId%3D34a02cf8f4414e29b15921876da36f9a%26responseType%3Dcode".to_string(),
            expects: LoginInputKind::AuthorizationCode as i32,
        });

        Ok(LoginChallenge {
            challenge_id,
            error: None,
            kind: Some(kind),
        })
    }

    async fn complete_login(
        &self,
        profile_id: &str,
        challenge_id: &str,
        user_input: &str,
    ) -> Result<LinkedAccount, Status> {
        let challenge = self
            .challenges
            .write()
            .await
            .remove(challenge_id)
            .ok_or_else(|| Status::not_found("Login challenge not found or already completed"))?;

        if challenge.created_at.elapsed() > Duration::from_secs(300) {
            return Err(Status::deadline_exceeded("Login challenge expired"));
        }

        if user_input.is_empty() {
            return Err(Status::invalid_argument("Authorization code is required"));
        }

        let mut egs = EpicGames::new();

        if !egs.auth_code(None, Some(user_input.to_owned())).await {
            return Err(Status::unauthenticated("Epic Games authorization failed"));
        }

        let user = egs.user_details();

        tracing::info!(
            "Logged in as {}",
            user.display_name.as_deref().unwrap_or("<unknown>")
        );

        let credentials = serde_json::to_vec(&user)
            .map_err(|e| Status::internal(format!("Failed to serialize Epic credentials: {e}")))?;

        self.credentials
            .save(profile_id, Storefront::EpicGames, &credentials)
            .await
            .map_err(|e| Status::internal(format!("Failed to save Epic credentials: {e}")))?;

        let account = egs.account_details().await;

        let (account_display_name, account_id) = match account {
            Some(account) => {
                tracing::info!("Display Name: {}", account.display_name);
                tracing::info!("Email: {}", account.email);
                tracing::info!("Country: {}", account.country);
                tracing::info!("2FA Enabled: {}", account.tfa_enabled);
                tracing::info!("Last Login: {}", account.last_login);

                (account.display_name, account.id)
            }

            None => {
                tracing::warn!("Epic authentication succeeded, but account_details() failed");

                (user.display_name.unwrap_or_default(), String::new())
            }
        };

        Ok(LinkedAccount {
            storefront: Storefront::EpicGames as i32,
            account_display_name,
            account_id,
            auth_state: AuthState::Active as i32,
            linked_at: None,
            last_verified_at: None,
            expires_at: None,
        })
    }

    async fn refresh_session(&self, profile_id: &str) -> Result<LinkedAccount, Status> {
        let credentials = self
            .credentials
            .load(profile_id, Storefront::EpicGames)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Epic Games credentials not found"))?;

        let user = serde_json::from_slice(&credentials)
            .map_err(|e| Status::internal(format!("Invalid Epic Games credentials: {e}")))?;

        let mut egs = EpicGames::new();

        egs.set_user_details(user);

        if !egs.login().await {
            return Err(Status::unauthenticated(
                "Epic Games session is no longer valid",
            ));
        }

        let user = egs.user_details();

        Ok(LinkedAccount {
            storefront: Storefront::EpicGames as i32,
            account_display_name: user.display_name.unwrap_or_default(),
            account_id: user.account_id.unwrap_or_default(),
            auth_state: AuthState::Active as i32,
            linked_at: None,
            last_verified_at: None,
            expires_at: None,
        })
    }

    async fn revoke_session(&self, _profile_id: &str) -> Result<(), Status> {
        Ok(())
    }

    async fn games(&self, profile_id: &str) -> Result<Vec<Game>, Status> {
        let credentials = self
            .credentials
            .load(profile_id, Storefront::EpicGames)
            .await
            .map_err(|e| Status::internal(e.to_string()))?
            .ok_or_else(|| Status::not_found("Epic Games credentials not found"))?;

        let user = serde_json::from_slice(&credentials)
            .map_err(|e| Status::internal(format!("Invalid Epic Games credentials: {e}")))?;

        let mut egs = EpicGames::new();
        egs.set_user_details(user);

        if !egs.login().await {
            return Err(Status::unauthenticated(
                "Epic Games session is no longer valid",
            ));
        }

        let assets = egs.list_assets(None, None).await;
        for asset in &assets {
            if let Some(info) = egs.asset_info(asset).await {
                println!("{}: {}", info.id, info.title.unwrap_or_default());
            }
        }

        Ok(assets
            .into_iter()
            .map(|asset| Game {
                id: asset.asset_id.clone(),
                title: asset.label_name.clone(),
                storefront: Storefront::EpicGames as i32,
                description: None,
                icon_url: None,
                cover_url: None,
            })
            .collect::<Vec<_>>())
    }
}
