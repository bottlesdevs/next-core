use std::process::ExitStatus;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::{path::PathBuf, process::Child, time::Duration};

use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use crate::runner::RunnerError;
use crate::{
    error::Result,
    proto::{
        BridgeHealthRequest, CreateProcessRequest, KillProcessRequest, ShutdownRequest,
        wine_bridge_client,
    },
    runner::{PrefixConfig, Runner, RunnerCommand},
};

static BRIDGE_ENDPOINT_MANAGER: LazyLock<BridgeEndpointManager> =
    LazyLock::new(|| BridgeEndpointManager::new());

#[derive(Error, Debug)]
pub enum BridgeError {
    #[error(
        "The WineBridge process exited with status {0} before it reported readiness over gRPC."
    )]
    BridgeExited(ExitStatus),
    #[error("WineBridge did not report readiness before the startup timeout elapsed.")]
    Timeout,
}

#[derive(Debug)]
struct BridgeEndpointManager {
    host: IpAddr,
    next_port: AtomicU16,
}

impl BridgeEndpointManager {
    fn new() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            next_port: AtomicU16::new(50051),
        }
    }

    fn next(&self) -> Result<Endpoint> {
        let port = self.next_port.fetch_add(1, Ordering::Relaxed);

        Endpoint::from_shared(format!("http://{}:{}", self.host, port)).map_err(Into::into)
    }
}

/// Managed client for a WineBridge server running inside a Wine prefix.
///
/// The wrapper starts WineBridge through a [`Runner`], waits until the gRPC
/// health endpoint reports ready, and then exposes higher-level methods for
/// process management through the generated WineBridge client.
///
/// Each client owns one spawned WineBridge process. Call [`shutdown`](Self::shutdown)
/// when the bridge is no longer needed so the server can stop cleanly.
pub struct WineBridgeClient {
    client: Mutex<wine_bridge_client::WineBridgeClient<Channel>>,
    _process: Child,
}

impl WineBridgeClient {
    /// Starts WineBridge inside `prefix` using `runner` and connects to it over gRPC.
    ///
    /// A loopback endpoint is allocated for the bridge and passed to the server
    /// process through `WINEBRIDGE_HOST` and `WINEBRIDGE_PORT`. The method returns
    /// only after WineBridge responds successfully to the health RPC.
    ///
    /// # Errors
    ///
    /// Returns an error if the runner command cannot be built, the bridge process
    /// cannot be spawned, the process exits before readiness, the startup timeout
    /// elapses, or the gRPC client cannot be created.
    pub async fn new(
        runner: &dyn Runner,
        prefix: &PrefixConfig,
        winebridge_executable: PathBuf,
    ) -> Result<Self> {
        // TODO: Don't unwrap here
        let endpoint = BRIDGE_ENDPOINT_MANAGER.next()?;
        let host = endpoint.uri().host().unwrap();
        let port = endpoint.uri().port().unwrap();

        let command = RunnerCommand::builder()
            .executable(winebridge_executable.display().to_string())
            .env("WINEBRIDGE_HOST", host)
            .env("WINEBRIDGE_PORT", &port.to_string())
            .build()
            .map_err(Into::<RunnerError>::into)?;

        let mut child = runner.run(prefix, command)?;

        let grpc_client =
            match Self::wait_until_ready(endpoint, &mut child, Duration::from_secs(30)).await {
                Ok(client) => client,
                Err(error) => {
                    if let Err(kill_error) = child.kill() {
                        tracing::debug!(
                            %kill_error,
                            "Failed to kill WineBridge process after startup failure"
                        );
                    }

                    if let Err(wait_error) = child.wait() {
                        tracing::debug!(
                            %wait_error,
                            "Failed to wait for WineBridge process after startup failure"
                        );
                    }

                    return Err(error);
                }
            };

        let client = Self {
            client: Mutex::new(grpc_client),
            _process: child,
        };

        Ok(client)
    }

    async fn wait_until_ready(
        endpoint: Endpoint,
        process: &mut Child,
        timeout: Duration,
    ) -> Result<wine_bridge_client::WineBridgeClient<Channel>> {
        let ready = async {
            loop {
                if let Some(status) = process.try_wait()? {
                    return Err(BridgeError::BridgeExited(status).into());
                }

                match wine_bridge_client::WineBridgeClient::connect(endpoint.clone()).await {
                    Ok(mut client) => match client.health(BridgeHealthRequest {}).await {
                        Ok(response) => {
                            if response.into_inner().ok {
                                return Ok(client);
                            }

                            tracing::debug!("WineBridge health check returned not ready");
                        }
                        Err(error) => {
                            tracing::debug!(%error, "WineBridge health check failed");
                        }
                    },
                    Err(error) => {
                        tracing::debug!(%error, "WineBridge connection attempt failed");
                    }
                }

                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };

        tokio::select! {
            result = ready => result,
            _ = tokio::time::sleep(timeout) => Err(BridgeError::Timeout.into()),
        }
    }

    // TODO: repalce `executable` with `LaunchRequest` struct that also contains args, work_dir etc.
    /// Launches a process through WineBridge and returns the process id reported by the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge rejects the launch.
    pub async fn launch_process(&self, executable: PathBuf) -> Result<u32> {
        let mut client = self.client.lock().await;
        let response = client
            .create_process(CreateProcessRequest {
                command: executable.display().to_string(),
                ..Default::default()
            })
            .await?;

        Ok(response.into_inner().pid)
    }

    /// Requests WineBridge to terminate a process by pid.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge cannot kill the
    /// target process.
    pub async fn kill_process(&self, pid: u32) -> Result<()> {
        let mut client = self.client.lock().await;

        client.kill_process(KillProcessRequest { pid }).await?;

        Ok(())
    }

    /// Requests the managed WineBridge server to shut down.
    ///
    /// This consumes the wrapper so callers cannot issue more RPCs after shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown RPC fails.
    pub async fn shutdown(self) -> Result<()> {
        let mut client = self.client.lock().await;

        client.shutdown(ShutdownRequest {}).await?;

        Ok(())
    }
}
