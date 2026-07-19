use std::{collections::HashMap, ffi::OsString, path::PathBuf};

use fvs_rs::Layer;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::BottleError;
use crate::compatibility::{
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
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) storage: PrefixStorage,
    #[serde(default)]
    pub(crate) programs: Vec<Program>,

    #[serde(flatten)]
    pub(crate) components: BottleComponents,
    #[serde(default)]
    pub(crate) dependencies: Vec<Dependency>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(crate) environment: HashMap<String, String>,

    // Runtime state
    #[serde(skip)]
    pub(crate) bridge: Option<WineBridgeClient>,
}

impl Bottle {
    pub(crate) fn new(
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
            environment: HashMap::new(),
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
        self.storage.kind()
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

    /// Stop WineBridge, wineserver, and prefix storage.
    pub async fn stop(&mut self) -> Result<()> {
        let mut first_error = None;
        if self.bridge.is_none() {
            match WineBridgeClient::connect_existing(&self.prefix_path()).await {
                Ok(bridge) => self.bridge = bridge,
                Err(error) => first_error = Some(error),
            }
        }
        match self.bridge.take() {
            Some(bridge) => {
                if let Err(error) = bridge.shutdown().await {
                    first_error.get_or_insert(error);
                }
            }
            None => {}
        }

        let runner = match self.load_runner() {
            Ok(runner) => Some(runner),
            Err(error) => {
                first_error.get_or_insert(error);

                None
            }
        };

        if let Some(runner) = runner.as_deref()
            && let Err(error) = runner.shutdown_prefix(&self.prefix_path())
        {
            first_error.get_or_insert(error);
        }

        if let Err(error) = self.storage.stop(&self.bottle_path()).await {
            first_error.get_or_insert(error);
        }

        first_error.map_or(Ok(()), Err)
    }

    pub async fn install_component(&mut self, component: &Component) -> Result<()> {
        match component.kind() {
            ComponentKind::Runner { kind } => self.install_runner(component, kind).await,
            ComponentKind::Winebridge => {
                if self.components.winebridge.id() == component.id() {
                    return Ok(());
                }
                self.stop().await?;
                self.components.winebridge = component.clone();
                self.save()
            }
            ComponentKind::Umu => {
                if self.runner().kind().runner_kind() != Some(RunnerKind::Proton) {
                    return Err(BottleError::WineRunnerWithUmu.into());
                }
                if self.components.umu.as_ref().map(Component::id) == Some(component.id()) {
                    return Ok(());
                }
                let previous = self.components.umu.replace(component.clone());
                if let Err(error) = self.stop().await {
                    self.components.umu = previous;
                    return Err(error);
                }
                self.save()
            }
            kind => self.install_prefix_component(component, kind).await,
        }
    }

    pub async fn uninstall_component(&mut self, id: Uuid) -> Result<Component> {
        if self.runner().id() == id
            || self.components.winebridge().id() == id
            || self
                .components
                .umu()
                .is_some_and(|component| component.id() == id)
        {
            return Err(BottleError::ComponentNotUninstallable(id).into());
        }

        let component = (&self.components)
            .into_iter()
            .find(|component| component.id() == id)
            .cloned()
            .ok_or(BottleError::ComponentNotInstalled(id))?;

        self.stop().await?;
        let previous_storage = self.storage.clone();
        let previous_environment = self.environment.clone();
        crate::compatibility::installer::uninstall(self, &component).await?;

        let removed = match component.kind() {
            ComponentKind::Dxvk => self.components.dxvk.take(),
            ComponentKind::Vkd3d => self.components.vkd3d.take(),
            ComponentKind::Nvapi => self.components.nvapi.take(),
            ComponentKind::LatencyFlex => self.components.latency_flex.take(),
            _ => unreachable!("only optional prefix components are uninstallable"),
        }
        .expect("the selected component is installed");

        if let Err(error) = self.save() {
            self.storage = previous_storage;
            self.environment = previous_environment;
            match component.kind() {
                ComponentKind::Dxvk => self.components.dxvk = Some(removed),
                ComponentKind::Vkd3d => self.components.vkd3d = Some(removed),
                ComponentKind::Nvapi => self.components.nvapi = Some(removed),
                ComponentKind::LatencyFlex => self.components.latency_flex = Some(removed),
                _ => unreachable!("only optional prefix components are uninstallable"),
            }
            return Err(error);
        }

        Ok(component)
    }

    async fn install_prefix_component(
        &mut self,
        component: &Component,
        kind: ComponentKind,
    ) -> Result<()> {
        let installed = match kind {
            ComponentKind::Dxvk => self.components.dxvk.as_ref(),
            ComponentKind::Vkd3d => self.components.vkd3d.as_ref(),
            ComponentKind::Nvapi => self.components.nvapi.as_ref(),
            ComponentKind::LatencyFlex => self.components.latency_flex.as_ref(),
            _ => {
                return Err(BottleError::InvalidPrefixComponent.into());
            }
        };
        if installed.map(Component::id) == Some(component.id()) {
            return Ok(());
        }
        let replaced_id = installed.map(Component::id);
        self.stop().await?;
        crate::compatibility::installer::execute(self, component, replaced_id).await?;
        match kind {
            ComponentKind::Dxvk => &mut self.components.dxvk,
            ComponentKind::Vkd3d => &mut self.components.vkd3d,
            ComponentKind::Nvapi => &mut self.components.nvapi,
            ComponentKind::LatencyFlex => &mut self.components.latency_flex,
            _ => unreachable!(),
        }
        .replace(component.clone());
        self.save()
    }

    pub async fn install_dependency(&mut self, dependency: &Dependency) -> Result<()> {
        if self
            .dependencies
            .iter()
            .any(|installed| installed.id() == dependency.id())
        {
            return Ok(());
        }
        self.stop().await?;
        crate::compatibility::installer::execute(self, dependency, None).await?;
        self.dependencies.push(dependency.clone());
        self.save()
    }

    async fn install_runner(&mut self, component: &Component, kind: RunnerKind) -> Result<()> {
        if self.runner().id() == component.id() {
            return Ok(());
        }
        let umu =
            crate::compatibility::installer::umu_for_runner(kind, self.components.umu.as_ref())?;
        let runner =
            crate::runner::load_runner(component.path(), kind, umu.as_ref().map(Component::path))?;
        self.stop().await?;
        let installed = (&self.components)
            .into_iter()
            .map(Component::id)
            .chain(self.dependencies.iter().map(Dependency::id))
            .collect::<Vec<_>>();
        self.storage
            .rebuild(runner.as_ref(), &component.id().to_string(), &installed)
            .await?;
        self.components.runner = component.clone();
        self.components.umu = umu;
        self.save()
    }

    pub(crate) fn load_runner(&self) -> Result<Box<dyn Runner>> {
        let kind = self
            .runner()
            .kind()
            .runner_kind()
            .ok_or(BottleError::RunnerComponentRequired)?;
        crate::runner::load_runner(
            self.runner().path(),
            kind,
            self.components.umu().map(Component::path),
        )
    }

    pub(crate) fn save(&self) -> Result<()> {
        next_config::save(self.bottle_path().join("bottle.toml"), self)?;
        Ok(())
    }

    pub(crate) fn bottle_path(&self) -> PathBuf {
        crate::utils::directories::expect().bottle(self.id())
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    pub(crate) async fn prefix(&mut self) -> Result<PathBuf> {
        let bottle_path = self.bottle_path();
        self.storage.prepare(&bottle_path).await?;
        Ok(bottle_path.join("prefix"))
    }

    pub(crate) async fn bridge(&mut self) -> Result<&WineBridgeClient> {
        if self.bridge.is_none() {
            let runner = self.load_runner()?;
            let prefix = self.prefix().await?;
            self.bridge = Some(
                WineBridgeClient::new(
                    runner.as_ref(),
                    &prefix,
                    self.components.winebridge().path().to_path_buf(),
                    self.environment
                        .iter()
                        .map(|(name, value)| (OsString::from(name), OsString::from(value))),
                )
                .await?,
            );
        }
        Ok(self.bridge.as_ref().expect("WineBridge was initialized"))
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct BottleComponents {
    pub(crate) runner: Component,
    pub(crate) winebridge: Component,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) umu: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dxvk: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) vkd3d: Option<Component>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nvapi: Option<Component>,
    #[serde(
        default,
        rename = "latency-flex",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) latency_flex: Option<Component>,
}

impl BottleComponents {
    pub fn new(
        runner: &Component,
        winebridge: &Component,
        umu: Option<&Component>,
    ) -> Result<Self> {
        let ComponentKind::Runner { kind } = runner.kind() else {
            return Err(BottleError::RunnerComponentRequired.into());
        };
        if winebridge.kind() != ComponentKind::Winebridge {
            return Err(BottleError::WinebridgeComponentRequired.into());
        }
        if umu.is_some_and(|component| component.kind() != ComponentKind::Umu) {
            return Err(BottleError::InvalidUmuComponent.into());
        }

        match (kind, umu) {
            (RunnerKind::Wine, Some(_)) => {
                return Err(BottleError::WineRunnerWithUmu.into());
            }
            (RunnerKind::Proton, None) => {
                return Err(BottleError::ProtonRunnerWithoutUmu.into());
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

impl<'a> IntoIterator for &'a BottleComponents {
    type Item = &'a Component;
    type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<&'a Component>, 4>>;

    fn into_iter(self) -> Self::IntoIter {
        [
            self.dxvk.as_ref(),
            self.vkd3d.as_ref(),
            self.nvapi.as_ref(),
            self.latency_flex.as_ref(),
        ]
        .into_iter()
        .flatten()
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
pub(crate) enum PrefixStorage {
    Standard,
    Virgo {
        #[serde(default)]
        layers: Vec<Layer>,
    },
}
