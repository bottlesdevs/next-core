use crate::plugins::egs::error::EpicGamesError;

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Epic Games error: {0}")]
    Epic(#[from] EpicGamesError),
}

impl From<PluginError> for tonic::Status {
    fn from(err: PluginError) -> Self {
        match err {
            PluginError::Epic(ref e) => tonic::Status::internal(format!("Epic Games error: {e}")),
        }
    }
}
