use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use thiserror::Error;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use crate::error::Result;
use crate::proto::fvs2d::{self as pb, fvs2d_client};

static VIRGO_ENDPOINT_MANAGER: LazyLock<VirgoEndpointManager> =
    LazyLock::new(VirgoEndpointManager::new);

#[derive(Error, Debug)]
pub enum VirgoError {
    #[error("The fvs2d process exited with status {0} before it reported readiness over gRPC.")]
    DaemonExited(ExitStatus),
    #[error("fvs2d did not report readiness before the startup timeout elapsed.")]
    Timeout,
}

#[derive(Debug)]
struct VirgoEndpointManager {
    host: IpAddr,
    next_port: AtomicU16,
}

impl VirgoEndpointManager {
    fn new() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            next_port: AtomicU16::new(50151),
        }
    }

    fn next(&self) -> Result<(Endpoint, IpAddr, u16)> {
        let host = self.host;
        let port = self.next_port.fetch_add(1, Ordering::Relaxed);
        let endpoint = Endpoint::from_shared(format!("http://{host}:{port}"))?;
        Ok((endpoint, host, port))
    }
}

/// Revision selector for a mount layer.
#[derive(Debug, Clone, Default)]
pub enum LayerRevision {
    /// The repository HEAD.
    #[default]
    Head,
    /// A state id or unique prefix.
    State(String),
    /// A branch name.
    Branch(String),
}

/// One lower layer of a mount, lowest to highest.
#[derive(Debug, Clone)]
pub struct Layer {
    pub repository: PathBuf,
    pub revision: LayerRevision,
}

impl Layer {
    pub fn new(repository: impl Into<PathBuf>) -> Self {
        Self {
            repository: repository.into(),
            revision: LayerRevision::Head,
        }
    }

    pub fn state(mut self, state: impl Into<String>) -> Self {
        self.revision = LayerRevision::State(state.into());
        self
    }

    pub fn branch(mut self, branch: impl Into<String>) -> Self {
        self.revision = LayerRevision::Branch(branch.into());
        self
    }

    fn into_proto(self) -> pb::Layer {
        let selector = match self.revision {
            LayerRevision::Head => None,
            LayerRevision::State(s) => Some(pb::commit_selector::Selector::StateIdOrPrefix(s)),
            LayerRevision::Branch(b) => Some(pb::commit_selector::Selector::Branch(b)),
        };
        pb::Layer {
            repository_path: self.repository.display().to_string(),
            revision: selector.map(|selector| pb::CommitSelector {
                selector: Some(selector),
            }),
        }
    }
}

/// Managed client for an fvs2d mount-manager daemon.
///
/// [`spawn`](Self::spawn) starts a dedicated daemon on a loopback endpoint and
/// waits until it answers the `Probe` RPC; [`connect`](Self::connect) attaches
/// to an already running daemon instead. A spawned daemon is stopped with
/// [`shutdown`](Self::shutdown).
pub struct VirgoDaemon {
    client: Mutex<fvs2d_client::Fvs2dClient<Channel>>,
    process: Option<Child>,
}

impl VirgoDaemon {
    /// Starts an fvs2d manager on a fresh loopback endpoint and connects to it.
    ///
    /// # Errors
    ///
    /// Returns an error if the daemon cannot be spawned, exits before
    /// readiness, or does not answer `Probe` within the startup timeout.
    pub async fn spawn(executable: impl Into<PathBuf>) -> Result<Self> {
        let (endpoint, host, port) = VIRGO_ENDPOINT_MANAGER.next()?;

        let mut child = Command::new(executable.into())
            .arg("-control")
            .arg(format!("tcp:{host}:{port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        let client = match Self::wait_until_ready(endpoint, &mut child, Duration::from_secs(15))
            .await
        {
            Ok(client) => client,
            Err(error) => {
                if let Err(kill_error) = child.kill() {
                    tracing::debug!(%kill_error, "Failed to kill fvs2d after startup failure");
                }
                if let Err(wait_error) = child.wait() {
                    tracing::debug!(%wait_error, "Failed to wait for fvs2d after startup failure");
                }
                return Err(error);
            }
        };

        Ok(Self {
            client: Mutex::new(client),
            process: Some(child),
        })
    }

    /// Connects to an fvs2d manager that is already running at `addr`
    /// (e.g. `http://127.0.0.1:50151`).
    ///
    /// # Errors
    ///
    /// Returns an error if the endpoint is invalid or the connection fails.
    pub async fn connect(addr: impl Into<String>) -> Result<Self> {
        let endpoint = Endpoint::from_shared(addr.into())?;
        let client = fvs2d_client::Fvs2dClient::connect(endpoint).await?;
        Ok(Self {
            client: Mutex::new(client),
            process: None,
        })
    }

    async fn wait_until_ready(
        endpoint: Endpoint,
        process: &mut Child,
        timeout: Duration,
    ) -> Result<fvs2d_client::Fvs2dClient<Channel>> {
        let ready = async {
            loop {
                if let Some(status) = process.try_wait()? {
                    return Err(VirgoError::DaemonExited(status).into());
                }

                match fvs2d_client::Fvs2dClient::connect(endpoint.clone()).await {
                    Ok(mut client) => match client.probe(()).await {
                        Ok(_) => return Ok(client),
                        Err(error) => {
                            tracing::debug!(%error, "fvs2d probe failed");
                        }
                    },
                    Err(error) => {
                        tracing::debug!(%error, "fvs2d connection attempt failed");
                    }
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };

        tokio::select! {
            result = ready => result,
            _ = tokio::time::sleep(timeout) => Err(VirgoError::Timeout.into()),
        }
    }

    /// Returns daemon version and host capability flags.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn probe(&self) -> Result<pb::ProbeResponse> {
        let mut client = self.client.lock().await;
        Ok(client.probe(()).await?.into_inner())
    }

    /// Initializes (or opens) an FVS repository at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn init_repository(&self, path: impl Into<PathBuf>) -> Result<pb::Repository> {
        let mut client = self.client.lock().await;
        let response = client
            .init_repository(pb::InitRepositoryRequest {
                repository_path: path.into().display().to_string(),
                block_size: 0,
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Creates a state (snapshot) of the repository at `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn commit(
        &self,
        path: impl Into<PathBuf>,
        message: impl Into<String>,
    ) -> Result<pb::Commit> {
        let mut client = self.client.lock().await;
        let response = client
            .commit(pb::CommitRequest {
                repository_path: path.into().display().to_string(),
                message: message.into(),
                allow_empty: false,
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Lists the saved states of the repository at `path`, newest first.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_commits(&self, path: impl Into<PathBuf>) -> Result<Vec<pb::Commit>> {
        let mut client = self.client.lock().await;
        let response = client
            .list_commits(pb::ListCommitsRequest {
                repository_path: path.into().display().to_string(),
            })
            .await?;
        Ok(response.into_inner().commits)
    }

    /// Restores a state into the repository working tree (exact checkout) and
    /// moves HEAD to it.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn rollback(
        &self,
        path: impl Into<PathBuf>,
        state: impl Into<String>,
    ) -> Result<pb::RestoreResponse> {
        let mut client = self.client.lock().await;
        let response = client
            .restore(pb::RestoreRequest {
                repository_path: path.into().display().to_string(),
                state_id_or_prefix: state.into(),
                destination_path: None,
                clean: true,
                reset: true,
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Creates a restore point: initializes the repository if needed and
    /// snapshots the current prefix content.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn create_restore_point(
        &self,
        prefix: impl Into<PathBuf>,
        label: impl Into<String>,
    ) -> Result<pb::Commit> {
        let prefix = prefix.into();
        self.init_repository(prefix.clone()).await?;
        self.commit(prefix, label).await
    }

    /// Mounts a stack of layers at `mount_point`, with an optional writable
    /// upper directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the daemon rejects it.
    pub async fn create_mount(
        &self,
        mount_point: impl Into<PathBuf>,
        layers: Vec<Layer>,
        upper: Option<PathBuf>,
    ) -> Result<pb::Mount> {
        let mut client = self.client.lock().await;
        let response = client
            .create_mount(pb::CreateMountRequest {
                spec: Some(pb::MountSpec {
                    mount_point: mount_point.into().display().to_string(),
                    layers: layers.into_iter().map(Layer::into_proto).collect(),
                    upper_path: upper.map(|p| p.display().to_string()),
                    debug: false,
                }),
            })
            .await?;
        Ok(response.into_inner())
    }

    /// Lists the mounts owned by the daemon.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_mounts(&self) -> Result<Vec<pb::Mount>> {
        let mut client = self.client.lock().await;
        Ok(client.list_mounts(()).await?.into_inner().mounts)
    }

    /// Unmounts a mount by id.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or the mount is unknown.
    pub async fn unmount(&self, mount_id: impl Into<String>, lazy: bool) -> Result<()> {
        let mode = if lazy {
            pb::UnmountMode::Lazy
        } else {
            pb::UnmountMode::Normal
        };
        let mut client = self.client.lock().await;
        client
            .unmount(pb::UnmountRequest {
                mount_id: mount_id.into(),
                mode: mode as i32,
            })
            .await?;
        Ok(())
    }

    /// Asks the daemon to unmount everything and exit, then reaps the spawned
    /// process if this client owns one.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown RPC fails.
    pub async fn shutdown(mut self) -> Result<()> {
        {
            let mut client = self.client.lock().await;
            client
                .shutdown(pb::ShutdownRequest {
                    mode: pb::UnmountMode::Normal as i32,
                })
                .await?;
        }
        if let Some(mut child) = self.process.take()
            && let Err(wait_error) = child.wait()
        {
            tracing::debug!(%wait_error, "Failed to wait for fvs2d after shutdown");
        }
        Ok(())
    }
}
