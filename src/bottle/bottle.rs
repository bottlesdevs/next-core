use std::{collections::HashMap, ffi::OsString, ops::AsyncFnOnce, path::PathBuf};

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
use crate::{
    error::Result,
    proto::Process,
    runner::{Runner, shutdown_prefix},
    winebridge::WineBridgeClient,
};

#[derive(Clone, Deserialize, Serialize, Config)]
#[config(version = 1)]
pub(crate) struct BottleConfig {
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
}

pub struct Bottle {
    pub(crate) config: BottleConfig,
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
        let mut bottle = Self {
            config: BottleConfig {
                id,
                name,
                components,
                dependencies,
                storage,
                programs: Vec::new(),
                environment: HashMap::new(),
            },
            bridge: None,
        };
        bottle.update(|_| Ok(()))?;
        Ok(bottle)
    }

    pub(crate) fn from_config(config: BottleConfig) -> Self {
        Self {
            config,
            bridge: None,
        }
    }

    pub fn id(&self) -> Uuid {
        self.config.id
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn components(&self) -> &BottleComponents {
        &self.config.components
    }

    pub fn runner(&self) -> &Component {
        self.config.components.runner()
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.config.dependencies
    }

    pub fn r#type(&self) -> BottleType {
        self.config.storage.kind()
    }

    pub fn programs(&self) -> &[Program] {
        &self.config.programs
    }

    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.config.programs.iter().find(|program| program.id == id)
    }

    pub fn add_program(&mut self, program: Program) -> Result<()> {
        if program.name.trim().is_empty() || program.executable.trim().is_empty() {
            return Err(BottleError::InvalidProgram.into());
        }

        self.update(move |draft| {
            draft.programs.push(program);
            Ok(())
        })
    }

    pub fn remove_program(&mut self, id: Uuid) -> Result<Program> {
        let mut removed = None;
        self.update(|draft| {
            let index = draft
                .programs
                .iter()
                .position(|program| program.id == id)
                .ok_or(BottleError::ProgramNotFound(id))?;
            removed = Some(draft.programs.remove(index));
            Ok(())
        })?;
        Ok(removed.expect("the program was removed from the draft"))
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
        if let Some(bridge) = self.bridge.take()
            && let Err(error) = bridge.shutdown().await
        {
            first_error.get_or_insert(error);
        }

        let runner = match self.load_runner() {
            Ok(runner) => Some(runner),
            Err(error) => {
                first_error.get_or_insert(error);

                None
            }
        };

        if let Some(runner) = runner.as_deref()
            && let Err(error) = shutdown_prefix(runner, &self.prefix_path())
        {
            first_error.get_or_insert(error);
        }

        if let Err(error) = self.config.storage.stop(&self.bottle_path()).await {
            first_error.get_or_insert(error);
        }

        first_error.map_or(Ok(()), Err)
    }

    /// Standard-prefix effects completed before a metadata error are not rolled back.
    pub async fn install_component(&mut self, component: &Component) -> Result<()> {
        match component.kind() {
            ComponentKind::Runner { kind } => self.install_runner(component, kind).await,
            ComponentKind::Winebridge => {
                if self.components().winebridge.id() == component.id() {
                    return Ok(());
                }
                self.stop().await?;
                self.update(|draft| {
                    draft.components.winebridge = component.clone();
                    Ok(())
                })
            }
            ComponentKind::Umu => {
                if self.runner().kind().runner_kind() != Some(RunnerKind::Proton) {
                    return Err(BottleError::WineRunnerWithUmu.into());
                }
                if self.components().umu.as_ref().map(Component::id) == Some(component.id()) {
                    return Ok(());
                }
                self.stop().await?;
                self.update(|draft| {
                    draft.components.umu = Some(component.clone());
                    Ok(())
                })
            }
            kind => self.install_prefix_component(component, kind).await,
        }
    }

    /// Standard-prefix effects completed before a metadata error are not rolled back.
    pub async fn uninstall_component(&mut self, id: Uuid) -> Result<Component> {
        if self.runner().id() == id
            || self.components().winebridge().id() == id
            || self
                .components()
                .umu()
                .is_some_and(|component| component.id() == id)
        {
            return Err(BottleError::ComponentNotUninstallable(id).into());
        }

        let component = self
            .components()
            .into_iter()
            .find(|component| component.id() == id)
            .cloned()
            .ok_or(BottleError::ComponentNotInstalled(id))?;

        self.stop().await?;
        let runner = self.load_runner()?;
        let winebridge = self.components().winebridge.path().to_path_buf();
        let bottle_path = self.bottle_path();
        self.update_after(
            async |storage, environment| {
                crate::compatibility::installer::uninstall(
                    crate::compatibility::installer::InstallInputs {
                        storage,
                        bottle_path: &bottle_path,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        environment,
                    },
                    &component,
                )
                .await
            },
            |draft| {
                draft
                    .components
                    .slot_mut(component.kind())?
                    .take()
                    .ok_or(BottleError::ComponentNotInstalled(id))?;
                Ok(())
            },
        )
        .await?;
        Ok(component)
    }

    async fn install_prefix_component(
        &mut self,
        component: &Component,
        kind: ComponentKind,
    ) -> Result<()> {
        let installed = self.components().slot(kind)?;
        if installed.map(Component::id) == Some(component.id()) {
            return Ok(());
        }
        let replaced_id = installed.map(Component::id);
        self.stop().await?;
        let runner = self.load_runner()?;
        let winebridge = self.components().winebridge.path().to_path_buf();
        let bottle_path = self.bottle_path();
        self.update_after(
            async |storage, environment| {
                crate::compatibility::installer::execute(
                    crate::compatibility::installer::InstallInputs {
                        storage,
                        bottle_path: &bottle_path,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        environment,
                    },
                    component,
                    replaced_id,
                )
                .await
            },
            |draft| {
                draft.components.slot_mut(kind)?.replace(component.clone());
                Ok(())
            },
        )
        .await
    }

    /// Standard-prefix effects completed before a metadata error are not rolled back.
    pub async fn install_dependency(&mut self, dependency: &Dependency) -> Result<()> {
        if self
            .dependencies()
            .iter()
            .any(|installed| installed.id() == dependency.id())
        {
            return Ok(());
        }
        self.stop().await?;
        let runner = self.load_runner()?;
        let winebridge = self.components().winebridge.path().to_path_buf();
        let bottle_path = self.bottle_path();
        self.update_after(
            async |storage, environment| {
                crate::compatibility::installer::execute(
                    crate::compatibility::installer::InstallInputs {
                        storage,
                        bottle_path: &bottle_path,
                        runner: runner.as_ref(),
                        winebridge: &winebridge,
                        environment,
                    },
                    dependency,
                    None,
                )
                .await
            },
            |draft| {
                draft.dependencies.push(dependency.clone());
                Ok(())
            },
        )
        .await
    }

    async fn install_runner(&mut self, component: &Component, kind: RunnerKind) -> Result<()> {
        if self.runner().id() == component.id() {
            return Ok(());
        }
        let umu =
            crate::compatibility::installer::umu_for_runner(kind, self.components().umu.as_ref())?;
        let runner =
            crate::runner::load_runner(component.path(), kind, umu.as_ref().map(Component::path))?;
        self.stop().await?;
        let installed = self
            .components()
            .into_iter()
            .map(Component::id)
            .chain(self.dependencies().iter().map(Dependency::id))
            .collect::<Vec<_>>();
        self.update_after(
            async |storage, _| {
                storage
                    .rebuild(runner.as_ref(), &component.id().to_string(), &installed)
                    .await
            },
            move |draft| {
                draft.components.runner = component.clone();
                draft.components.umu = umu;
                Ok(())
            },
        )
        .await
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
            self.components().umu().map(Component::path),
        )
    }

    fn update<F>(&mut self, update: F) -> Result<()>
    where
        F: FnOnce(&mut BottleConfig) -> Result<()>,
    {
        let mut draft = self.config.clone();
        update(&mut draft)?;
        next_config::save(self.bottle_path().join("bottle.toml"), &draft)?;
        self.config = draft;
        Ok(())
    }

    async fn update_after<E, C>(&mut self, effect: E, commit: C) -> Result<()>
    where
        E: for<'a> AsyncFnOnce(
            &'a mut PrefixStorage,
            &'a mut HashMap<String, String>,
        ) -> Result<()>,
        C: FnOnce(&mut BottleConfig) -> Result<()>,
    {
        let mut storage = self.config.storage.clone();
        let mut environment = self.config.environment.clone();
        effect(&mut storage, &mut environment).await?;
        self.update(move |draft| {
            draft.storage = storage;
            draft.environment = environment;
            commit(draft)
        })
    }

    pub(crate) fn bottle_path(&self) -> PathBuf {
        crate::utils::directories::expect().bottle(self.id())
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    pub(crate) async fn prefix(&mut self) -> Result<PathBuf> {
        let bottle_path = self.bottle_path();
        self.config.storage.prepare(&bottle_path).await?;
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
                    self.components().winebridge().path().to_path_buf(),
                    self.config
                        .environment
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

    fn slot(&self, kind: ComponentKind) -> Result<Option<&Component>> {
        match kind {
            ComponentKind::Dxvk => Ok(self.dxvk.as_ref()),
            ComponentKind::Vkd3d => Ok(self.vkd3d.as_ref()),
            ComponentKind::Nvapi => Ok(self.nvapi.as_ref()),
            ComponentKind::LatencyFlex => Ok(self.latency_flex.as_ref()),
            _ => Err(BottleError::InvalidPrefixComponent.into()),
        }
    }

    fn slot_mut(&mut self, kind: ComponentKind) -> Result<&mut Option<Component>> {
        match kind {
            ComponentKind::Dxvk => Ok(&mut self.dxvk),
            ComponentKind::Vkd3d => Ok(&mut self.vkd3d),
            ComponentKind::Nvapi => Ok(&mut self.nvapi),
            ComponentKind::LatencyFlex => Ok(&mut self.latency_flex),
            _ => Err(BottleError::InvalidPrefixComponent.into()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_effect_does_not_publish_working_config() {
        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            "/runner",
        )
        .unwrap();
        let winebridge = Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
        let mut bottle = Bottle::from_config(BottleConfig {
            id: Uuid::new_v4(),
            name: "test".into(),
            storage: PrefixStorage::Standard,
            programs: Vec::new(),
            components: BottleComponents::new(&runner, &winebridge, None).unwrap(),
            dependencies: Vec::new(),
            environment: HashMap::new(),
        });

        let result = bottle
            .update_after(
                async |storage, environment| {
                    *storage = PrefixStorage::Virgo { layers: Vec::new() };
                    environment.insert("CHANGED".into(), "yes".into());
                    Err(BottleError::InvalidProgram.into())
                },
                |_| Ok(()),
            )
            .await;

        assert!(result.is_err());
        assert!(matches!(bottle.config.storage, PrefixStorage::Standard));
        assert!(bottle.config.environment.is_empty());
    }

    #[test]
    fn component_slots_have_explicit_failure_semantics() {
        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            "/runner",
        )
        .unwrap();
        let winebridge = Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
        let mut components = BottleComponents::new(&runner, &winebridge, None).unwrap();

        for kind in [
            ComponentKind::Dxvk,
            ComponentKind::Vkd3d,
            ComponentKind::Nvapi,
            ComponentKind::LatencyFlex,
        ] {
            assert!(components.slot(kind).unwrap().is_none());
            let component = Component::new(kind, "test", "/component").unwrap();
            let id = component.id();
            components.slot_mut(kind).unwrap().replace(component);
            assert_eq!(components.slot(kind).unwrap().map(Component::id), Some(id));
        }

        for kind in [
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            ComponentKind::Winebridge,
            ComponentKind::Umu,
        ] {
            assert!(matches!(
                components.slot(kind),
                Err(crate::error::Error::Bottle(
                    BottleError::InvalidPrefixComponent
                ))
            ));
            assert!(matches!(
                components.slot_mut(kind),
                Err(crate::error::Error::Bottle(
                    BottleError::InvalidPrefixComponent
                ))
            ));
        }
    }
}
