use std::path::PathBuf;

use fvs_rs::{Layer, UnmountMode};
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{error::Result, proto::Process, runner::Runner, winebridge::WineBridgeClient};

use super::{
    error::BottleError,
    manager::{config, fvs},
    virgo::ensure_empty_dir,
};

#[derive(Deserialize, Serialize, Config)]
#[config(version = 1)]
pub struct Bottle {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) runner: String,
    pub(super) storage: PrefixStorage,
    #[serde(default)]
    pub(super) programs: Vec<Program>,

    // Runtime state
    #[serde(skip)]
    pub(super) bridge: Option<WineBridgeClient>,
}

impl Bottle {
    pub(super) fn new(
        id: Uuid,
        name: String,
        runner: String,
        storage: PrefixStorage,
    ) -> Result<Self> {
        let bottle = Self {
            id,
            name,
            runner,
            storage,
            programs: Vec::new(),
            bridge: None,
        };
        bottle.save()?;
        Ok(bottle)
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn runner(&self) -> &str {
        &self.runner
    }

    pub fn r#type(&self) -> BottleType {
        match self.storage {
            PrefixStorage::Standard => BottleType::Standard,
            PrefixStorage::Virgo { .. } => BottleType::Virgo,
        }
    }

    pub fn programs(&self) -> &[Program] {
        &self.programs
    }

    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.programs.iter().find(|program| program.id == id)
    }

    pub fn add_program(&mut self, program: Program) -> Result<()> {
        if program.name.trim().is_empty() || program.executable.trim().is_empty() {
            return Err(BottleError::InvalidProgram.into());
        }

        self.programs.push(program);
        self.save()
    }

    pub fn remove_program(&mut self, id: Uuid) -> Result<Program> {
        let index = self
            .programs
            .iter()
            .position(|program| program.id == id)
            .ok_or(BottleError::ProgramNotFound(id))?;
        let program = self.programs.remove(index);
        self.save()?;
        Ok(program)
    }

    pub async fn run(&mut self, id: Uuid) -> Result<u32> {
        let program = self
            .program(id)
            .cloned()
            .ok_or(BottleError::ProgramNotFound(id))?;
        self.bridge()
            .await?
            .launch_process(
                program.id,
                program.executable,
                program.args,
                program.working_directory,
                program.new_console,
            )
            .await
    }

    pub async fn processes(&mut self) -> Result<Vec<Process>> {
        self.bridge().await?.list_processes().await
    }

    pub async fn kill(&mut self, id: Uuid) -> Result<()> {
        if self.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        self.bridge().await?.kill_process(id).await
    }

    /// Stop WineBridge, wineserver, and the Virgo mount.
    pub async fn stop(&mut self) -> Result<()> {
        let mut first_error = None;
        if let Some(bridge) = self.bridge.take()
            && let Err(error) = bridge.shutdown().await
        {
            first_error = Some(error);
        }

        let runner = match self.load_runner() {
            Ok(runner) => Some(runner),
            Err(error) => {
                first_error.get_or_insert(error);

                None
            }
        };

        if let Some(runner) = runner.as_deref()
            && let Err(error) = runner.shutdown_prefix(&self.prefix())
        {
            first_error.get_or_insert(error);
        }

        if matches!(self.storage, PrefixStorage::Virgo { .. })
            && let Err(error) = self.unmount().await
        {
            first_error.get_or_insert(error);
        }

        first_error.map_or(Ok(()), Err)
    }

    fn load_runner(&self) -> Result<Box<dyn Runner>> {
        crate::runner::load_runner(
            &crate::directories::expect().runner(&self.runner),
            config().umu_executable.as_deref(),
        )
    }

    fn save(&self) -> Result<()> {
        next_config::save(self.root().join("bottle.toml"), self)?;
        Ok(())
    }

    fn root(&self) -> PathBuf {
        crate::directories::expect().bottle(self.id())
    }

    fn prefix(&self) -> PathBuf {
        self.root().join("prefix")
    }

    async fn ensure_mounted(&mut self) -> Result<()> {
        let root = self.root();
        let prefix = self.prefix();
        let mountpoint = prefix.as_path();
        if let PrefixStorage::Virgo { layers } = self.storage.clone() {
            let mounted = fvs().await?.list_mounts().await?.into_iter().any(|mount| {
                mount
                    .spec
                    .as_ref()
                    .is_some_and(|spec| spec.mount_point == mountpoint.display().to_string())
            });
            if !mounted {
                ensure_empty_dir(mountpoint)?;
                fvs()
                    .await?
                    .mount(mountpoint, layers, Some(root.join("upper")))
                    .await?;
            }
        }

        Ok(())
    }

    async fn bridge(&mut self) -> Result<&WineBridgeClient> {
        if self.bridge.is_none() {
            let runner = self.load_runner()?;
            self.ensure_mounted().await?;
            let prefix = self.prefix();
            self.bridge = Some(
                WineBridgeClient::new(
                    runner.as_ref(),
                    &prefix,
                    config().winebridge_executable.clone(),
                )
                .await?,
            );
        }
        Ok(self.bridge.as_ref().expect("WineBridge was initialized"))
    }

    async fn unmount(&mut self) -> Result<()> {
        let mountpoint = self.prefix().display().to_string();
        let mount = fvs().await?.list_mounts().await?.into_iter().find(|mount| {
            mount
                .spec
                .as_ref()
                .is_some_and(|spec| spec.mount_point == mountpoint)
        });
        if let Some(mount) = mount {
            fvs().await?.unmount(&mount, UnmountMode::Normal).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Program {
    pub id: Uuid,
    pub name: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
    #[serde(default)]
    pub new_console: bool,
}

impl Program {
    pub fn new(name: impl Into<String>, executable: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            executable: executable.into(),
            args: Vec::new(),
            working_directory: None,
            new_console: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum BottleType {
    Standard,
    Virgo,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub(super) enum PrefixStorage {
    Standard,
    Virgo {
        #[serde(default)]
        layers: Vec<Layer>,
    },
}
