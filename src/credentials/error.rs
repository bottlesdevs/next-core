#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential store error: {0}")]
    Store(#[from] keyring::Error),

    #[error("credential not found")]
    NotFound,

    #[error("invalid credential data: {0}")]
    InvalidData(String),
}
