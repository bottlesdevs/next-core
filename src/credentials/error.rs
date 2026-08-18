#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("credential store error: {0}")]
    Store(#[from] keyring::Error),
}
