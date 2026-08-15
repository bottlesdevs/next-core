use crate::error::CredentialError;

#[derive(Debug, thiserror::Error)]
pub enum EpicGamesError {
    #[error("Epic Games login challenge not found or already completed")]
    LoginChallengeNotFound,
    #[error("Epic Games login challenge expired")]
    LoginChallengeExpired,
    #[error("Epic Games authorization code is required")]
    AuthorizationCodeRequired,
    #[error("Epic API error: {0}")]
    Api(#[from] egs_api::api::error::EpicAPIError),
    #[error("Epic Games authorization failed")]
    AuthorizationFailed,
    #[error("Epic Games session is no longer valid")]
    SessionInvalid,
    #[error("Epic Games credentials error: {0}")]
    Credentials(#[from] CredentialError),
    #[error("Epic Games JSON error: {0}")]
    Json(#[from] serde_json::Error),
}
