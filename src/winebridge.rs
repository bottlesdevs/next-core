//! `WineBridge` discovery, process supervision, and gRPC requests.
//!
//! Each Wine prefix has at most one discovered server. The discovery file
//! supplies only a TCP port; clients always connect over the IPv4 loopback
//! interface. An unreachable discovered server must be stopped or cleaned up
//! before another server can start.

use std::{
    io,
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use async_io::Timer;
use async_process::Child;
use futures_lite::future;
use thiserror::Error;
use tonic::transport::{Channel, Endpoint};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};

use crate::{
    command::{Command, Spawnable},
    error::Result,
    runner::Runner,
    utils::fs::exists,
};
use crate::{
    proto::{self, wine_bridge_client::WineBridgeClient as GrpcClient},
    runner::RunnerCommand,
};

use crate::proto::{
    DllOverride, DllOverrideMode, Drive, PathInfo, Process, RegistryHive, RegistryKey, Service,
    ServiceStartType, WinebootMode, registry_value::Value as RegistryValue,
};

/// Failures specific to starting or supervising `WineBridge`.
///
/// Protocol transport and status failures use the corresponding variants of
/// [`crate::error::Error`].
#[derive(Error, Debug)]
pub enum BridgeError {
    /// The child process exited before the health service became ready.
    #[error(
        "The WineBridge process exited with status {0} before it reported readiness over gRPC."
    )]
    BridgeExited(ExitStatus),
    /// Startup exceeded the `WineBridge` readiness deadline.
    #[error("WineBridge did not report readiness before the startup timeout elapsed.")]
    Timeout,
    /// A discovery file exists, but its endpoint cannot be reached.
    #[error(
        "WineBridge discovery exists but the runtime is unreachable at {0}; call stop() and retry"
    )]
    Unavailable(PathBuf),
    /// `WineBridge` did not terminate within the shutdown deadline.
    #[error("WineBridge did not stop before the shutdown timeout elapsed.")]
    ShutdownTimeout,
    /// `WineBridge` returned a response that violated the expected protocol shape.
    #[error("WineBridge returned an invalid response: {0}")]
    InvalidResponse(&'static str),
}

const PORT_FILE_NAME: &str = "bottles-winebridge.port";

/// Reads and validates `WineBridge`'s loopback endpoint discovery file.
///
/// # Errors
///
/// Returns an I/O error when the file cannot be read, or
/// [`BridgeError::InvalidResponse`] when it does not contain a nonzero TCP port.
async fn endpoint_from_port_file(path: &Path) -> Result<Option<Endpoint>> {
    let port = match async_fs::read_to_string(path).await {
        Ok(port) => port,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let port = port
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
        .ok_or(BridgeError::InvalidResponse(
            "WineBridge published an invalid port; call stop() and retry",
        ))?;
    Ok(Some(Endpoint::from_shared(format!(
        "http://127.0.0.1:{port}"
    ))?))
}

/// Managed client for a `WineBridge` server running inside a Wine prefix.
///
/// Startup waits for the gRPC health endpoint to report ready. Dropping a client
/// does not stop the server; it remains available until the owning environment
/// is stopped.
pub(crate) struct WineBridgeClient {
    client: GrpcClient<Channel>,
    port_file: PathBuf,
}

impl WineBridgeClient {
    /// Builds the runner command used to start `WineBridge` inside `prefix`.
    ///
    /// `env_vars` are applied as host-process environment overrides, and the
    /// Windows-side discovery file path is supplied through
    /// `WINEBRIDGE_PORT_FILE`.
    pub(crate) fn command<'a>(
        runner: &dyn Runner,
        prefix: &Path,
        winebridge_root: impl AsRef<Path>,
        env_vars: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> RunnerCommand {
        runner.command(
            prefix,
            Command::new(winebridge_root.as_ref().join("bottles-winebridge.exe"))
                .envs(env_vars)
                .env(
                    "WINEBRIDGE_PORT_FILE",
                    format!(r"C:\windows\temp\{PORT_FILE_NAME}"),
                ),
        )
    }

    /// Reuses a healthy discovered server or spawns `command` and waits for it.
    ///
    /// An existing but unhealthy discovery file is treated as
    /// [`BridgeError::Unavailable`]; it is not silently replaced by a second
    /// server.
    ///
    /// # Errors
    ///
    /// Returns discovery and transport errors, process-spawn errors, or the
    /// startup failures described by [`BridgeError`].
    pub(crate) async fn connect_or_spawn(prefix: &Path, command: impl Spawnable) -> Result<Self> {
        if let Some(client) = Self::try_connect(prefix).await? {
            return Ok(client);
        }

        Self::connect(prefix, command.spawn()?).await
    }

    /// Waits for a spawned process to publish a healthy endpoint.
    ///
    /// Readiness is polled until the process exits or 30 seconds elapse. On
    /// failure, the child is killed and reaped before this method returns.
    ///
    /// # Errors
    ///
    /// Returns [`BridgeError::BridgeExited`] if the process exits before
    /// readiness, [`BridgeError::Timeout`] after the deadline, or an I/O,
    /// discovery, transport, kill, or wait error.
    async fn connect(prefix: &Path, mut process: Child) -> Result<Self> {
        let ready = async {
            loop {
                if let Some(status) = process.try_status()? {
                    if let Some(client) = Self::probe(prefix).await? {
                        return Ok(client);
                    }
                    return Err(BridgeError::BridgeExited(status).into());
                }

                if let Some(client) = Self::probe(prefix).await? {
                    return Ok(client);
                }

                Timer::after(Duration::from_millis(100)).await;
            }
        };

        let result = future::race(ready, async {
            Timer::after(Duration::from_secs(30)).await;
            Err(BridgeError::Timeout.into())
        })
        .await;
        if result.is_err() {
            if let Err(error) = process.kill()
                && error.kind() != io::ErrorKind::InvalidInput
            {
                return Err(error.into());
            }
            process.status().await?;
        }
        result
    }

    /// Connects to the discovered server without starting a new one.
    ///
    /// Returns `None` only when no discovery file exists. If a discovery file
    /// exists but its endpoint is unreachable or unhealthy, returns
    /// [`BridgeError::Unavailable`] instead of starting a second server.
    ///
    /// # Errors
    ///
    /// Returns an I/O, transport, discovery-format, or unavailable-runtime error.
    pub(crate) async fn try_connect(prefix: &Path) -> Result<Option<Self>> {
        let bridge = Self::probe(prefix).await?;
        if bridge.is_none() && exists(&Self::port_file(prefix)).await? {
            return Err(BridgeError::Unavailable(prefix.to_owned()).into());
        }
        Ok(bridge)
    }

    /// Shuts down the discovered server, if one is running.
    ///
    /// # Errors
    ///
    /// Returns discovery errors or errors from [`shutdown`](Self::shutdown).
    pub(crate) async fn shutdown_existing(prefix: &Path) -> Result<()> {
        if let Some(bridge) = Self::try_connect(prefix).await? {
            bridge.shutdown().await?;
        }
        Ok(())
    }

    /// Removes the `WineBridge` discovery file if it exists.
    ///
    /// # Errors
    ///
    /// Returns an I/O error other than a missing file.
    pub(crate) async fn clear_discovery(prefix: &Path) -> Result<()> {
        match async_fs::remove_file(Self::port_file(prefix)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Probes the discovered endpoint and requires a serving health response.
    ///
    /// Missing discovery, connection failure, timeout, non-serving health, and
    /// health-RPC failure are represented as `None`. Discovery file I/O and
    /// format failures are returned.
    ///
    /// # Errors
    ///
    /// Returns an error if the discovery file cannot be read or contains an
    /// invalid endpoint.
    async fn probe(prefix: &Path) -> Result<Option<Self>> {
        let port_file = Self::port_file(prefix);

        let Some(endpoint) = endpoint_from_port_file(&port_file).await? else {
            return Ok(None);
        };

        let Ok(channel) = endpoint
            .connect_timeout(Duration::from_secs(2))
            .connect()
            .await
        else {
            return Ok(None);
        };

        let mut request = tonic::Request::new(HealthCheckRequest {
            service: proto::wine_bridge_server::SERVICE_NAME.to_string(),
        });
        request.set_timeout(Duration::from_secs(2));
        let response = HealthClient::new(channel.clone()).check(request).await;

        Ok(matches!(response, Ok(response) if response.get_ref().status() == ServingStatus::Serving)
            .then(|| Self {
                client: GrpcClient::new(channel),
                port_file,
            }))
    }

    fn port_file(prefix: &Path) -> PathBuf {
        prefix.join("drive_c/windows/temp").join(PORT_FILE_NAME)
    }

    // --- Process Management ---

    /// Lists processes tracked by this `WineBridge` server.
    ///
    /// # Errors
    ///
    /// Returns a gRPC status or transport error if the request fails.
    pub async fn list_processes(&self) -> Result<Vec<Process>> {
        let mut client = self.client.clone();
        let response = client.list_processes(()).await?.into_inner();

        Ok(response.processes)
    }

    /// Launches a process and returns its Wine process identifier.
    ///
    /// `id` identifies the process group used by [`kill_process`](Self::kill_process).
    /// The remaining values are forwarded to `WineBridge` unchanged.
    ///
    /// # Errors
    ///
    /// Returns a gRPC status or transport error if `WineBridge` rejects or cannot
    /// complete the launch request.
    pub async fn launch_process(
        &self,
        id: uuid::Uuid,
        executable: String,
        arguments: Vec<String>,
        working_directory: Option<String>,
        new_console: bool,
    ) -> Result<u32> {
        let mut client = self.client.clone();
        let response = client
            .launch_process(proto::LaunchProcessRequest {
                id: id.to_string(),
                executable,
                arguments,
                working_directory,
                new_console,
            })
            .await?;

        Ok(response.into_inner().pid)
    }

    /// Terminates the process group identified by `id`.
    ///
    /// # Errors
    ///
    /// Returns a gRPC status or transport error if the request fails.
    pub async fn kill_process(&self, id: uuid::Uuid) -> Result<()> {
        let mut client = self.client.clone();

        client
            .kill_process(proto::KillProcessRequest { id: id.to_string() })
            .await?;

        Ok(())
    }

    // --- Registry Management ---

    /// Creates a registry key under `hive`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn create_registry_key(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .create_registry_key(proto::RegistryKeyRequest {
                hive: hive as i32,
                subkey: subkey.into(),
            })
            .await?;

        Ok(())
    }

    /// Recursively deletes a registry key and all of its descendants.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn delete_registry_tree(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_registry_tree(proto::RegistryKeyRequest {
                hive: hive as i32,
                subkey: subkey.into(),
            })
            .await?;

        Ok(())
    }

    /// Returns a registry key together with its values.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_registry_key(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
    ) -> Result<RegistryKey> {
        let mut client = self.client.clone();
        let response = client
            .get_registry_key(proto::RegistryKeyRequest {
                hive: hive as i32,
                subkey: subkey.into(),
            })
            .await?;

        Ok(response.into_inner())
    }

    /// Returns a single value stored under a registry key.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_registry_value(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<RegistryValue> {
        let mut client = self.client.clone();
        let response = client
            .get_registry_value(proto::RegistryValueRequest {
                hive: hive as i32,
                subkey: subkey.into(),
                name: name.into(),
            })
            .await?
            .into_inner();

        response
            .value
            .ok_or_else(|| BridgeError::InvalidResponse("registry value is missing").into())
    }

    /// Creates or replaces a value under a registry key.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn set_registry_value(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
        name: impl Into<String>,
        value: RegistryValue,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .set_registry_value(proto::SetRegistryValueRequest {
                hive: hive as i32,
                subkey: subkey.into(),
                name: name.into(),
                value: Some(proto::RegistryValue { value: Some(value) }),
            })
            .await?;

        Ok(())
    }

    /// Deletes a value from a registry key.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn delete_registry_value(
        &self,
        hive: RegistryHive,
        subkey: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_registry_value(proto::RegistryValueRequest {
                hive: hive as i32,
                subkey: subkey.into(),
                name: name.into(),
            })
            .await?;

        Ok(())
    }

    // --- File System ---

    /// Creates a directory (and any missing parents) inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn create_directory(&self, path: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .create_directory(proto::PathRequest { path: path.into() })
            .await?;

        Ok(())
    }

    /// Deletes a file or directory inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn delete_file(&self, path: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_file(proto::PathRequest { path: path.into() })
            .await?;

        Ok(())
    }

    /// Recursively deletes a directory and all of its descendants.
    ///
    /// # Errors
    ///
    /// Returns an error if the path is not a directory or the operation fails.
    pub async fn delete_directory_tree(&self, path: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_directory_tree(proto::PathRequest { path: path.into() })
            .await?;

        Ok(())
    }

    /// Copies a file inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn copy_file(
        &self,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .copy_file(proto::PathTransferRequest {
                source: source.into(),
                destination: destination.into(),
            })
            .await?;

        Ok(())
    }

    /// Moves or renames a file inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn move_path(
        &self,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .move_path(proto::PathTransferRequest {
                source: source.into(),
                destination: destination.into(),
            })
            .await?;

        Ok(())
    }

    /// Returns metadata for a file or directory.
    ///
    /// # Errors
    ///
    /// Returns `NOT_FOUND` when the path does not exist.
    pub async fn path_info(&self, path: impl Into<String>) -> Result<PathInfo> {
        let mut client = self.client.clone();
        Ok(client
            .get_path_info(proto::PathRequest { path: path.into() })
            .await?
            .into_inner())
    }

    /// Lists the entries of a directory inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_directory(&self, path: impl Into<String>) -> Result<Vec<PathInfo>> {
        let mut client = self.client.clone();
        let response = client
            .list_directory(proto::PathRequest { path: path.into() })
            .await?
            .into_inner();

        Ok(response.entries)
    }

    /// Checks whether a path exists.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn exists(&self, path: impl Into<String>) -> Result<bool> {
        let mut client = self.client.clone();
        match client
            .get_path_info(proto::PathRequest { path: path.into() })
            .await
        {
            Ok(_) => Ok(true),
            Err(error) if error.code() == tonic::Code::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    // --- Service Management ---

    /// Lists the Windows services registered in the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_services(&self) -> Result<Vec<Service>> {
        let mut client = self.client.clone();
        let response = client.list_services(()).await?.into_inner();

        Ok(response.services)
    }

    /// Returns a service and its current configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_service(&self, name: impl Into<String>) -> Result<Service> {
        let mut client = self.client.clone();
        Ok(client
            .get_service(proto::ServiceRequest { name: name.into() })
            .await?
            .into_inner())
    }

    /// Starts a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn start_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .start_service(proto::ServiceRequest { name: name.into() })
            .await?;

        Ok(())
    }

    /// Stops a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn stop_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .stop_service(proto::ServiceRequest { name: name.into() })
            .await?;

        Ok(())
    }

    /// Creates a new service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn create_service(
        &self,
        name: impl Into<String>,
        display_name: impl Into<String>,
        binary_path: impl Into<String>,
        start_type: ServiceStartType,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .create_service(proto::CreateServiceRequest {
                name: name.into(),
                display_name: display_name.into(),
                binary_path: binary_path.into(),
                start_type: start_type as i32,
            })
            .await?;

        Ok(())
    }

    /// Deletes a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn delete_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_service(proto::ServiceRequest { name: name.into() })
            .await?;

        Ok(())
    }

    // --- DLL Overrides ---

    /// Lists the configured DLL overrides. A missing override key yields an empty list.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_dll_overrides(&self) -> Result<Vec<DllOverride>> {
        let mut client = self.client.clone();
        match client.list_dll_overrides(()).await {
            Ok(response) => Ok(response.into_inner().overrides),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(Vec::new()),
            Err(status) => Err(status.into()),
        }
    }

    /// Returns the override mode configured for a single DLL.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_dll_override(&self, dll: impl Into<String>) -> Result<DllOverride> {
        let mut client = self.client.clone();
        let response = client
            .get_dll_override(proto::DllOverrideRequest { dll: dll.into() })
            .await?;

        Ok(response.into_inner())
    }

    /// Sets the override mode for a DLL.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn set_dll_override(
        &self,
        dll: impl Into<String>,
        mode: DllOverrideMode,
    ) -> Result<()> {
        let mut client = self.client.clone();
        client
            .set_dll_override(proto::SetDllOverrideRequest {
                dll: dll.into(),
                mode: mode as i32,
            })
            .await?;

        Ok(())
    }

    /// Removes a DLL override. A missing override is already removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn delete_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        match client
            .delete_dll_override(proto::DllOverrideRequest { dll: dll.into() })
            .await
        {
            Ok(_) => Ok(()),
            Err(status) if status.code() == tonic::Code::NotFound => Ok(()),
            Err(status) => Err(status.into()),
        }
    }

    // --- System ---

    /// Runs `wineboot` in the prefix with the requested mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or `WineBridge` reports failure.
    pub async fn wineboot(&self, mode: WinebootMode) -> Result<()> {
        let mut client = self.client.clone();
        client
            .run_wineboot(proto::WinebootRequest { mode: mode as i32 })
            .await?;

        Ok(())
    }

    /// Returns information about the drives mapped in the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_drives(&self) -> Result<Vec<Drive>> {
        let mut client = self.client.clone();
        let response = client.list_drives(()).await?.into_inner();

        Ok(response.drives)
    }

    /// Requests the managed `WineBridge` server to shut down.
    ///
    /// After the shutdown RPC succeeds, this waits up to five seconds for the
    /// server to remove its discovery file.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown RPC or discovery-file check fails, or
    /// [`BridgeError::ShutdownTimeout`] if the file remains after the deadline.
    pub async fn shutdown(&self) -> Result<()> {
        let mut client = self.client.clone();
        let mut request = tonic::Request::new(());
        request.set_timeout(Duration::from_secs(5));
        client.shutdown(request).await?;
        drop(client);
        for _ in 0..50 {
            if !exists(&self.port_file).await? {
                return Ok(());
            }
            Timer::after(Duration::from_millis(100)).await;
        }
        Err(BridgeError::ShutdownTimeout.into())
    }
}
