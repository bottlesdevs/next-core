use crate::error::Result;
use std::path::Path;

/// Managed client for an fvs2d mount-manager daemon.
///
/// [`spawn`](Self::spawn) starts a dedicated daemon on a loopback endpoint and
/// waits until it answers the `Probe` RPC; [`connect`](Self::connect) attaches
/// to an already running daemon instead. A spawned daemon is stopped with
/// [`shutdown`](Self::shutdown).
pub struct VirgoDaemon(fvs_rs::Fvs2dClient);

impl std::ops::Deref for VirgoDaemon {
    type Target = fvs_rs::Fvs2dClient;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl VirgoDaemon {
    /// Starts an fvs2d manager on a fresh loopback endpoint and connects to it.
    ///
    /// # Errors
    ///
    /// Returns an error if the daemon cannot be spawned, exits before
    /// readiness, or does not answer `Probe` within the startup timeout.
    pub async fn new(executable: impl AsRef<Path>) -> Result<Self> {
        let client = fvs_rs::Fvs2dClient::new(executable).await?;

        Ok(VirgoDaemon(client))
    }

    /// Creates a restore point: initializes the repository if needed and
    /// snapshots the current prefix content.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn create_restore_point(
        &self,
        prefix: impl AsRef<Path>,
        label: impl Into<String>,
    ) -> Result<fvs_rs::Commit> {
        let repository = self.new_repository(prefix, 4096).await?;
        let commit = self.commit(&repository, label.into()).await?;

        Ok(commit)
    }
}
