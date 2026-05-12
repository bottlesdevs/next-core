use std::process::ExitStatus;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::{path::PathBuf, process::Child, time::Duration};

use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use crate::proto::ShutdownRequest;
use crate::runner::RunnerError;
use crate::{
    error::Result,
    proto::{BridgeHealthRequest, CreateProcessRequest, KillProcessRequest, wine_bridge_client},
    runner::{PrefixConfig, Runner, RunnerCommand},
};

static BRIDGE_ENDPOINT_MANAGER: LazyLock<BridgeEndpointManager> =
    LazyLock::new(|| BridgeEndpointManager::new());

#[derive(Error, Debug)]
pub enum BridgeError {
    BridgeExited(ExitStatus),
    Timeout,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::BridgeExited(status) => {
                write!(f, "WineBridge exited before becoming ready: {}", status)
            }
            BridgeError::Timeout => write!(
                f,
                "Timeout occured while waiting for Winebridge gRPC server to start"
            ),
        }
    }
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

pub struct WineBridgeClient {
    client: Mutex<wine_bridge_client::WineBridgeClient<Channel>>,
    process: Child,
}

impl WineBridgeClient {
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
            Self::wait_until_ready(endpoint, &mut child, Duration::from_secs(3)).await?;

        let client = Self {
            client: Mutex::new(grpc_client),
            process: child,
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

                if let Ok(mut client) =
                    wine_bridge_client::WineBridgeClient::connect(endpoint.clone()).await
                {
                    if let Ok(response) = client.health(BridgeHealthRequest {}).await {
                        if response.into_inner().ok {
                            return Ok(client);
                        }
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

    pub async fn kill_process(&self, pid: u32) -> Result<()> {
        let mut client = self.client.lock().await;

        client.kill_process(KillProcessRequest { pid }).await?;

        Ok(())
    }

    pub async fn shutdown(self) -> Result<()> {
        let mut client = self.client.lock().await;

        client.shutdown(ShutdownRequest {}).await?;

        Ok(())
    }
}
