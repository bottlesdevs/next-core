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
use tokio::sync::Mutex;
use tonic::transport::{Channel, Endpoint};

use crate::proto::{self, wine_bridge_client::WineBridgeClient as GrpcClient};
use crate::runner::RunnerError;
use crate::{
    error::Result,
    runner::{PrefixConfig, Runner, RunnerCommand},
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
    #[error("WineBridge reported an operation failure: {0}")]
    OperationFailed(String),
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
pub struct LaunchRequest(proto::CreateProcessRequest);

impl LaunchRequest {
    /// Creates a launch request for `executable` with no arguments.
    pub fn new(executable: impl AsRef<Path>) -> Self {
        Self(proto::CreateProcessRequest {
            command: executable.as_ref().display().to_string(),
            ..Default::default()
        })
    }

    /// Appends a single argument passed after the executable.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.0.args.push(arg.into());
        self
    }

    /// Appends multiple arguments in order.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.0.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Sets the working directory the process is started in.
    pub fn work_dir(mut self, work_dir: impl AsRef<Path>) -> Self {
        self.0.work_dir = Some(work_dir.as_ref().display().to_string());
        self
    }

    /// Sets the working directory from an optional value, leaving it unset on `None`.
    pub fn maybe_work_dir(mut self, work_dir: Option<PathBuf>) -> Self {
        self.0.work_dir = work_dir.map(|dir| dir.display().to_string());
        self
    }

    /// Requests the process be launched with a new console window.
    pub fn terminal(mut self, terminal: bool) -> Self {
        self.0.terminal = terminal;
        self
    }
}

impl From<LaunchRequest> for proto::CreateProcessRequest {
    fn from(request: LaunchRequest) -> Self {
        request.0
    }
}

/// Maps a [`proto::MessageResponse`] onto a `Result`, surfacing the reported error.
fn check_message(response: proto::MessageResponse) -> Result<()> {
    if response.success {
        Ok(())
    } else {
        Err(BridgeError::OperationFailed(response.error.unwrap_or_default()).into())
    }
}

/// Maps a [`proto::FileOperationResponse`] onto a `Result`, surfacing the reported error.
fn check_file_operation(response: proto::FileOperationResponse) -> Result<()> {
    if response.success {
        Ok(())
    } else {
        Err(BridgeError::OperationFailed(response.error).into())
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
    client: Mutex<GrpcClient<Channel>>,
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
            client: Mutex::new(grpc_client),
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

                match GrpcClient::connect(endpoint.clone()).await {
                    Ok(mut client) => match client.health(proto::BridgeHealthRequest {}).await {
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

    // --- Basic Communication ---

    /// Sends a free-form message to WineBridge and confirms it was accepted.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn message(&self, message: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .message(proto::MessageRequest {
                message: message.into(),
            })
            .await?
            .into_inner();

        check_message(response)
    }

    // --- Process Management ---

    /// Returns the processes currently running inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn running_processes(&self) -> Result<Vec<proto::Process>> {
        let mut client = self.client.lock().await;
        let response = client
            .running_processes(proto::RunningProcessesRequest {})
            .await?
            .into_inner();

        Ok(response.processes)
    }

    /// Launches a process through WineBridge and returns the process id reported by the server.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge rejects the launch.
    pub async fn launch_process(&self, request: LaunchRequest) -> Result<u32> {
        let mut client = self.client.lock().await;
        let response = client
            .create_process(proto::CreateProcessRequest::from(request))
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
        hive: impl Into<String>,
        subkey: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .create_registry_key(proto::CreateRegistryKeyRequest {
                hive: hive.into(),
                subkey: subkey.into(),
            })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Deletes a registry key under `hive`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_registry_key(
        &self,
        hive: impl Into<String>,
        subkey: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .delete_registry_key(proto::DeleteRegistryKeyRequest {
                hive: hive.into(),
                subkey: subkey.into(),
            })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Returns a registry key together with its values.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_registry_key(
        &self,
        hive: impl Into<String>,
        subkey: impl Into<String>,
    ) -> Result<proto::RegistryKey> {
        let mut client = self.client.lock().await;
        let response = client
            .get_registry_key(proto::GetRegistryKeyRequest {
                hive: hive.into(),
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
    pub async fn get_registry_key_value(
        &self,
        hive: impl Into<String>,
        subkey: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<proto::RegistryValue> {
        let mut client = self.client.lock().await;
        let response = client
            .get_registry_key_value(proto::RegistryKeyRequest {
                hive: hive.into(),
                subkey: subkey.into(),
                name: name.into(),
            })
            .await?;

        Ok(response.into_inner())
    }

    /// Creates or replaces a value under a registry key.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn set_registry_key_value(
        &self,
        hive: impl Into<String>,
        subkey: impl Into<String>,
        name: impl Into<String>,
        value_type: proto::RegistryValueType,
        data: Vec<u8>,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .set_registry_key_value(proto::SetRegistryKeyValueRequest {
                key: Some(proto::RegistryKeyRequest {
                    hive: hive.into(),
                    subkey: subkey.into(),
                    name: name.into(),
                }),
                value: Some(proto::RegistryValue {
                    r#type: value_type as i32,
                    data,
                }),
            })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Deletes a value from a registry key.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_registry_key_value(
        &self,
        hive: impl Into<String>,
        subkey: impl Into<String>,
        name: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .delete_registry_key_value(proto::RegistryKeyRequest {
                hive: hive.into(),
                subkey: subkey.into(),
                name: name.into(),
            })
            .await?
            .into_inner();

        check_message(response)
    }

    // --- File System ---

    /// Creates a directory (and any missing parents) inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn create_directory(&self, path: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .create_directory(proto::FileOperationRequest { path: path.into() })
            .await?
            .into_inner();

        check_file_operation(response)
    }

    /// Deletes a file or directory inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_file(&self, path: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .delete_file(proto::FileOperationRequest { path: path.into() })
            .await?
            .into_inner();

        check_file_operation(response)
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
        let mut client = self.client.lock().await;
        let response = client
            .copy_file(proto::CopyMoveRequest {
                source: source.into(),
                destination: destination.into(),
            })
            .await?
            .into_inner();

        check_file_operation(response)
    }

    /// Moves or renames a file inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn move_file(
        &self,
        source: impl Into<String>,
        destination: impl Into<String>,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .move_file(proto::CopyMoveRequest {
                source: source.into(),
                destination: destination.into(),
            })
            .await?
            .into_inner();

        check_file_operation(response)
    }

    /// Lists the entries of a directory inside the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_directory(&self, path: impl Into<String>) -> Result<Vec<proto::FileInfo>> {
        let mut client = self.client.lock().await;
        let response = client
            .list_directory(proto::FileOperationRequest { path: path.into() })
            .await?
            .into_inner();

        Ok(response.files)
    }

    /// Checks whether a path exists, returning `(exists, is_dir)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn exists(&self, path: impl Into<String>) -> Result<(bool, bool)> {
        let mut client = self.client.lock().await;
        let response = client
            .exists(proto::FileOperationRequest { path: path.into() })
            .await?
            .into_inner();

        Ok((response.exists, response.is_dir))
    }

    // --- Service Management ---

    /// Lists the Windows services registered in the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_services(&self) -> Result<Vec<proto::ServiceInfo>> {
        let mut client = self.client.lock().await;
        let response = client
            .list_services(proto::ListServicesRequest {})
            .await?
            .into_inner();

        Ok(response.services)
    }

    /// Returns the current state of a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_service_status(&self, name: impl Into<String>) -> Result<proto::ServiceState> {
        let mut client = self.client.lock().await;
        let response = client
            .get_service_status(proto::ServiceRequest { name: name.into() })
            .await?
            .into_inner();

        Ok(response.state())
    }

    /// Starts a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn start_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .start_service(proto::ServiceRequest { name: name.into() })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Stops a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn stop_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .stop_service(proto::ServiceRequest { name: name.into() })
            .await?
            .into_inner();

        check_message(response)
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
        start_type: proto::ServiceStartType,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .create_service(proto::CreateServiceRequest {
                name: name.into(),
                display_name: display_name.into(),
                binary_path: binary_path.into(),
                start_type: start_type as i32,
            })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Deletes a service.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_service(&self, name: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .delete_service(proto::ServiceRequest { name: name.into() })
            .await?
            .into_inner();

        check_message(response)
    }

    // --- DLL Overrides ---

    /// Lists the configured DLL overrides.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn list_dll_overrides(&self) -> Result<Vec<proto::DllOverride>> {
        let mut client = self.client.lock().await;
        let response = client
            .list_dll_overrides(proto::ListDllOverridesRequest {})
            .await?
            .into_inner();

        Ok(response.overrides)
    }

    /// Returns the override mode configured for a single DLL.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_dll_override(
        &self,
        dll: impl Into<String>,
    ) -> Result<proto::DllOverrideResponse> {
        let mut client = self.client.lock().await;
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
        mode: proto::DllOverrideMode,
    ) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .set_dll_override(proto::SetDllOverrideRequest {
                dll: dll.into(),
                mode: mode as i32,
            })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Removes a DLL override.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn delete_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .delete_dll_override(proto::DllOverrideRequest { dll: dll.into() })
            .await?
            .into_inner();

        check_message(response)
    }

    // --- System ---

    /// Runs `wineboot` in the prefix with the requested mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails or WineBridge reports failure.
    pub async fn wineboot(&self, mode: proto::WinebootMode) -> Result<()> {
        let mut client = self.client.lock().await;
        let response = client
            .wineboot(proto::WinebootRequest { mode: mode as i32 })
            .await?
            .into_inner();

        check_message(response)
    }

    /// Returns information about the drives mapped in the prefix.
    ///
    /// # Errors
    ///
    /// Returns an error if the gRPC request fails.
    pub async fn get_drive_info(&self) -> Result<Vec<proto::Drive>> {
        let mut client = self.client.lock().await;
        let response = client
            .get_drive_info(proto::DriveInfoRequest {})
            .await?
            .into_inner();

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
        let mut client = self.client.lock().await;

        client.shutdown(proto::ShutdownRequest {}).await?;

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

        let proto: proto::CreateProcessRequest = request.into();

        assert_eq!(proto.command, "C:\\app.exe");
        assert_eq!(proto.args, vec!["--flag", "a", "b"]);
        assert_eq!(proto.work_dir.as_deref(), Some("C:\\work"));
        assert!(proto.terminal);
    }

    #[test]
    fn launch_request_defaults_leave_work_dir_unset() {
        let proto: proto::CreateProcessRequest = LaunchRequest::new("game.exe").into();

        assert_eq!(proto.command, "game.exe");
        assert!(proto.args.is_empty());
        assert_eq!(proto.work_dir, None);
        assert!(!proto.terminal);
    }

    #[test]
    fn maybe_work_dir_overrides_with_none() {
        let proto: proto::CreateProcessRequest = LaunchRequest::new("game.exe")
            .work_dir("C:\\work")
            .maybe_work_dir(None)
            .into();

        assert_eq!(proto.work_dir, None);
    }

    #[test]
    fn check_message_reports_error_text() {
        let err = check_message(proto::MessageResponse {
            success: false,
            error: Some("boom".to_string()),
        })
        .unwrap_err();

        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn check_file_operation_success_is_ok() {
        assert!(
            check_file_operation(proto::FileOperationResponse {
                success: true,
                error: String::new(),
            })
            .is_ok()
        );
    }
}
