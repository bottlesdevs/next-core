use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    time::Duration,
};

use thiserror::Error;
use tonic::transport::{Channel, Endpoint};
use tonic_health::pb::{
    HealthCheckRequest, health_check_response::ServingStatus, health_client::HealthClient,
};

use crate::proto::{self, wine_bridge_client::WineBridgeClient as GrpcClient};
use crate::{
    error::Result,
    runner::{Runner, RunnerCommand},
};

use crate::proto::{
    DllOverride, DllOverrideMode, Drive, PathInfo, Process, RegistryHive, RegistryKey, Service,
    ServiceStartType, WinebootMode, registry_value::Value as RegistryValue,
};

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

fn endpoint_from_port_file(path: &Path) -> Result<Option<Endpoint>> {
    let port = match fs::read_to_string(path) {
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
            "WineBridge published an invalid port",
        ))?;
    Ok(Some(Endpoint::from_shared(format!(
        "http://127.0.0.1:{port}"
    ))?))
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
    process: Option<Child>,
}

impl Drop for WineBridgeClient {
    fn drop(&mut self) {
        if let Some(mut process) = self.process.take() {
            let _ = process.kill();
            let _ = process.wait();
        }
    }
}

impl WineBridgeClient {
    /// Starts WineBridge inside `prefix` using `runner` and connects to it over gRPC.
    ///
    /// WineBridge binds an OS-assigned loopback port and publishes it through the
    /// prefix before this method connects and waits for a successful health RPC.
    ///
    /// # Errors
    ///
    /// Returns an error if the bridge process cannot be spawned, exits before
    /// readiness, times out during startup, or the gRPC client cannot be created.
    pub async fn new(
        runner: &dyn Runner,
        prefix: &Path,
        winebridge_executable: PathBuf,
        environment: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Result<Self> {
        let port_file_name = format!("{}.port", uuid::Uuid::new_v4());
        let port_file = prefix.join("drive_c/windows/temp").join(&port_file_name);
        fs::create_dir_all(port_file.parent().expect("port file has a parent"))?;

        let command = RunnerCommand::new(winebridge_executable)
            .env(
                "WINEBRIDGE_PORT_FILE",
                format!(r"C:\windows\temp\{port_file_name}"),
            )
            .envs(environment);

        let mut child = runner.run(prefix, command)?;

        let grpc_client =
            match Self::wait_until_ready(&port_file, &mut child, Duration::from_secs(30)).await {
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

                    let _ = fs::remove_file(&port_file);
                    let _ = fs::remove_file(port_file.with_extension("tmp"));
                    return Err(error);
                }
            };
        let _ = fs::remove_file(port_file);

        let client = Self {
            client: grpc_client,
            process: Some(child),
        };

        Ok(client)
    }

    async fn wait_until_ready(
        port_file: &Path,
        process: &mut Child,
        timeout: Duration,
    ) -> Result<GrpcClient<Channel>> {
        let ready = async {
            let mut endpoint = None;
            loop {
                if let Some(status) = process.try_wait()? {
                    return Err(BridgeError::BridgeExited(status).into());
                }

                if endpoint.is_none() {
                    endpoint = endpoint_from_port_file(port_file)?;
                }
                if let Some(endpoint) = &endpoint {
                    match endpoint.connect().await {
                        Ok(channel) => match HealthClient::new(channel.clone())
                            .check(HealthCheckRequest {
                                service: proto::wine_bridge_server::SERVICE_NAME.to_string(),
                            })
                            .await
                        {
                            Ok(response)
                                if response.get_ref().status() == ServingStatus::Serving =>
                            {
                                return Ok(GrpcClient::new(channel));
                            }
                            Ok(_) => {
                                tracing::debug!("WineBridge health check returned not serving")
                            }
                            Err(error) => {
                                tracing::debug!(%error, "WineBridge health check failed");
                            }
                        },
                        Err(error) => {
                            tracing::debug!(%error, "WineBridge connection attempt failed");
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

    // --- Process Management ---

    pub async fn list_processes(&self) -> Result<Vec<Process>> {
        let mut client = self.client.clone();
        let response = client.list_processes(()).await?.into_inner();

        Ok(response.processes)
    }

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
    pub async fn shutdown(mut self) -> Result<()> {
        let mut process = self.process.take().expect("WineBridge process is present");
        let mut client = self.client.clone();
        if let Err(error) = client.shutdown(()).await {
            let _ = process.kill();
            let _ = process.wait();
            return Err(error.into());
        }
        for _ in 0..50 {
            if process.try_wait()?.is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = process.kill();
        process.wait()?;
        Ok(())
    }
}
