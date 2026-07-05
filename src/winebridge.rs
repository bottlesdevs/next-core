use std::process::ExitStatus;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::{
    path::{Path, PathBuf},
    process::Child,
    time::Duration,
};

use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;
use tonic::transport::{Channel, Endpoint};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};

use crate::proto::{self, wine_bridge_client::WineBridgeClient as GrpcClient};
use crate::runner::RunnerError;
use crate::{
    error::Result,
    runner::{PrefixConfig, Runner, RunnerCommand},
};

pub use crate::proto::{
    DllOverride, DllOverrideMode, Drive, PathInfo, PathKind, Process, RegistryHive, RegistryKey,
    RegistryKeyValue, RegistryMultiString, Service, ServiceStartType, ServiceState, WinebootMode,
    registry_value::Value as RegistryValue,
};

static BRIDGE_ENDPOINT_MANAGER: LazyLock<BridgeEndpointManager> =
    LazyLock::new(BridgeEndpointManager::new);

#[derive(Error, Debug)]
pub enum BridgeError {
    #[error(
        "The WineBridge process exited with status {0} before it reported readiness over gRPC."
    )]
    BridgeExited(ExitStatus),
    #[error("WineBridge did not report readiness before the startup timeout elapsed.")]
    Timeout,
    #[error("WineBridge returned an invalid response: {0}")]
    InvalidResponse(&'static str),
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
        let host = self.host;
        let port = self.next_port.fetch_add(1, Ordering::Relaxed);
        Ok(Endpoint::from_shared(format!("http://{host}:{port}"))?)
    }
}

/// Description of a process launch issued through WineBridge.
///
/// Use [`LaunchRequest::new`] with the Windows executable path and chain the
/// builder-style setters to attach arguments, a working directory, or request a
/// dedicated console. The request maps directly onto the WineBridge
/// `CreateProcess` RPC.
#[derive(Debug, Clone)]
pub struct LaunchRequest(proto::LaunchProcessRequest);

impl LaunchRequest {
    /// Creates a launch request for `executable` with no arguments.
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self(proto::LaunchProcessRequest {
            executable: executable.as_ref().display().to_string(),
            ..Default::default()
        })
    }

    /// Appends a single argument passed after the executable.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.0.arguments.push(arg.into());
        self
    }

    /// Appends multiple arguments in order.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.0.arguments.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory the process is started in.
    pub fn work_dir(mut self, work_dir: impl AsRef<Path>) -> Self {
        self.0.working_directory = Some(work_dir.as_ref().display().to_string());
        self
    }

    /// Sets the working directory from an optional value, leaving it unset on `None`.
    pub fn maybe_work_dir(mut self, work_dir: Option<PathBuf>) -> Self {
        self.0.working_directory = work_dir.map(|dir| dir.display().to_string());
        self
    }

    /// Requests the process be launched with a new console window.
    pub fn terminal(mut self, terminal: bool) -> Self {
        self.0.new_console = terminal;
        self
    }
}

impl From<LaunchRequest> for proto::LaunchProcessRequest {
    fn from(request: LaunchRequest) -> Self {
        request.0
    }
}

/// Managed client for a WineBridge server running inside a Wine prefix.
///
/// The wrapper starts WineBridge through a [`Runner`], waits until the gRPC
/// health endpoint reports ready, and then exposes higher-level methods for
/// every WineBridge capability through the generated client.
///
/// Each client owns one spawned WineBridge process. Call [`shutdown`](Self::shutdown)
/// when the bridge is no longer needed so the server can stop cleanly.
pub struct WineBridgeClient {
    client: GrpcClient<Channel>,
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
        let endpoint = BRIDGE_ENDPOINT_MANAGER.next()?;
        let host = endpoint.uri().host().expect("bridge endpoint has a host");
        let port = endpoint
            .uri()
            .port_u16()
            .expect("bridge endpoint has a port");

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
            client: grpc_client,
            _process: child,
        };

        Ok(client)
    }

    async fn wait_until_ready(
        endpoint: Endpoint,
        process: &mut Child,
        timeout: Duration,
    ) -> Result<GrpcClient<Channel>> {
        let ready = async {
            loop {
                if let Some(status) = process.try_wait()? {
                    return Err(BridgeError::BridgeExited(status).into());
                }

                match endpoint.connect().await {
                    Ok(channel) => match HealthClient::new(channel.clone())
                        .check(HealthCheckRequest {
                            service: proto::wine_bridge_server::SERVICE_NAME.to_string(),
                        })
                        .await
                    {
                        Ok(response) if response.get_ref().status() == ServingStatus::Serving => {
                            return Ok(GrpcClient::new(channel));
                        }
                        Ok(_) => tracing::debug!("WineBridge health check returned not serving"),
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

    // --- Process Management ---

    /// Returns the processes currently running inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_processes(&self) -> Result<Vec<Process>> {
        let mut client = self.client.clone();
        let response = client.list_processes(()).await?.into_inner();

        Ok(response.processes)
    }

    /// Launches a process through WineBridge and returns the process id reported by the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge rejects the launch.
    pub async fn launch_process(&self, request: LaunchRequest) -> Result<u32> {
        let mut client = self.client.clone();
        let response = client
            .launch_process(proto::LaunchProcessRequest::from(request))
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
        let mut client = self.client.clone();

        client
            .kill_process(proto::KillProcessRequest { pid })
            .await?;

        Ok(())
    }

    // --- Registry Management ---

    /// Creates a registry key under `hive`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_service(proto::ServiceRequest { name: name.into() })
            .await?;

        Ok(())
    }

    // --- DLL Overrides ---

    /// Lists the configured DLL overrides.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_dll_overrides(&self) -> Result<Vec<DllOverride>> {
        let mut client = self.client.clone();
        let response = client.list_dll_overrides(()).await?.into_inner();

        Ok(response.overrides)
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
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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

    /// Removes a DLL override.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let mut client = self.client.clone();
        client
            .delete_dll_override(proto::DllOverrideRequest { dll: dll.into() })
            .await?;

        Ok(())
    }

    // --- System ---

    /// Runs `wineboot` in the prefix with the requested mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
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

    /// Requests the managed WineBridge server to shut down.
    ///
    /// This consumes the wrapper so callers cannot issue more RPCs after shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown RPC fails.
    pub async fn shutdown(self) -> Result<()> {
        let mut client = self.client.clone();

        client.shutdown(()).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_request_maps_all_fields_to_proto() {
        let request = LaunchRequest::new("C:\\app.exe")
            .arg("--flag")
            .args(["a", "b"])
            .work_dir("C:\\work")
            .terminal(true);

        let proto: proto::LaunchProcessRequest = request.into();

        assert_eq!(proto.executable, "C:\\app.exe");
        assert_eq!(proto.arguments, vec!["--flag", "a", "b"]);
        assert_eq!(proto.working_directory.as_deref(), Some("C:\\work"));
        assert!(proto.new_console);
    }

    #[test]
    fn launch_request_defaults_leave_work_dir_unset() {
        let proto: proto::LaunchProcessRequest = LaunchRequest::new("game.exe").into();

        assert_eq!(proto.executable, "game.exe");
        assert!(proto.arguments.is_empty());
        assert_eq!(proto.working_directory, None);
        assert!(!proto.new_console);
    }

    #[test]
    fn maybe_work_dir_overrides_with_none() {
        let proto: proto::LaunchProcessRequest = LaunchRequest::new("game.exe")
            .work_dir("C:\\work")
            .maybe_work_dir(None)
            .into();

        assert_eq!(proto.working_directory, None);
    }
}
