use std::path::PathBuf;

use fvs_rs::{Layer, UnmountMode};
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{error::BottleError, manager::fvs, virgo::ensure_empty_dir};
use crate::{
    components::{
        Component,
        catalog::{ComponentKind, RunnerKind},
    },
    dependencies::Dependency,
};
use crate::{error::Result, proto::Process, runner::Runner, winebridge::WineBridgeClient};

#[derive(Deserialize, Serialize, Config)]
#[config(version = 1)]
pub struct Bottle {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) storage: PrefixStorage,
    #[serde(default)]
    pub(super) programs: Vec<Program>,

    #[serde(flatten)]
    pub(super) components: BottleComponents,
    #[serde(default)]
    pub(super) dependencies: Vec<Dependency>,

    // Runtime state
    #[serde(skip)]
    pub(super) bridge: Option<WineBridgeClient>,
}

impl Bottle {
    pub(super) fn new(
        id: Uuid,
        name: String,
        components: BottleComponents,
        dependencies: Vec<Dependency>,
        storage: PrefixStorage,
    ) -> Result<Self> {
        let bottle = Self {
            id,
            name,
            components,
            dependencies,
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

    pub fn components(&self) -> &BottleComponents {
        &self.components
    }

    pub fn runner(&self) -> &Component {
        self.components.runner()
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
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
        let kind = self
            .runner()
            .kind()
            .runner_kind()
            .ok_or_else(|| invalid_components("runner component is required"))?;
        crate::runner::load_runner(
            self.runner().path(),
            kind,
            self.components.umu().map(Component::path),
        )
    }

    fn save(&self) -> Result<()> {
        next_config::save(self.root().join("bottle.toml"), self)?;
        Ok(())
    }

    fn root(&self) -> PathBuf {
        crate::utils::directories::expect().bottle(self.id())
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
                    self.components.winebridge().path().to_path_buf(),
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
pub struct BottleComponents {
    runner: Component,
    winebridge: Component,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    umu: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dxvk: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vkd3d: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nvapi: Option<Component>,
    #[serde(
        default,
        rename = "latency-flex",
        skip_serializing_if = "Option::is_none"
    )]
    latency_flex: Option<Component>,
}

impl BottleComponents {
    pub fn new(
        runner: &Component,
        winebridge: &Component,
        umu: Option<&Component>,
    ) -> Result<Self> {
        let ComponentKind::Runner { kind } = runner.kind() else {
            return Err(invalid_components("runner component is required"));
        };
        if winebridge.kind() != ComponentKind::Winebridge {
            return Err(invalid_components("WineBridge component is required"));
        }
        if umu.is_some_and(|component| component.kind() != ComponentKind::Umu) {
            return Err(invalid_components("UMU component has the wrong kind"));
        }

        match (kind, umu) {
            (RunnerKind::Wine, Some(_)) => {
                return Err(invalid_components("Wine runner must not use UMU"));
            }
            (RunnerKind::Proton, None) => {
                return Err(invalid_components("Proton runner requires UMU"));
            }
            _ => {}
        }

        Ok(Self {
            runner: runner.clone(),
            winebridge: winebridge.clone(),
            umu: umu.cloned(),
            dxvk: None,
            vkd3d: None,
            nvapi: None,
            latency_flex: None,
        })
    }

    pub fn with(mut self, component: &Component) -> Result<Self> {
        let slot = match component.kind() {
            ComponentKind::Dxvk => &mut self.dxvk,
            ComponentKind::Vkd3d => &mut self.vkd3d,
            ComponentKind::Nvapi => &mut self.nvapi,
            ComponentKind::LatencyFlex => &mut self.latency_flex,
            _ => {
                return Err(invalid_components(
                    "only optional bottle components can be added",
                ));
            }
        };
        if slot.is_some() {
            return Err(invalid_components("component kind is already installed"));
        }
        *slot = Some(component.clone());
        Ok(self)
    }

    pub fn runner(&self) -> &Component {
        &self.runner
    }

    pub fn winebridge(&self) -> &Component {
        &self.winebridge
    }

    pub fn umu(&self) -> Option<&Component> {
        self.umu.as_ref()
    }

    pub fn dxvk(&self) -> Option<&Component> {
        self.dxvk.as_ref()
    }

    pub fn vkd3d(&self) -> Option<&Component> {
        self.vkd3d.as_ref()
    }

    pub fn nvapi(&self) -> Option<&Component> {
        self.nvapi.as_ref()
    }

    pub fn latency_flex(&self) -> Option<&Component> {
        self.latency_flex.as_ref()
    }
}

fn invalid_components(message: impl Into<String>) -> crate::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into()).into()
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
