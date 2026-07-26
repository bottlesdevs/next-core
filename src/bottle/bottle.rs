use std::{future::Future, ops::AsyncFnOnce, path::PathBuf};

use fvs_rs::Layer;
use next_config::Config;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{edit::BottleEdit, error::BottleError};
use crate::{
    Context, Operation,
    compatibility::{
        components::{Component, catalog::ComponentKind},
        dependencies::Dependency,
        installer::{InstallResource, Installable},
    },
    error::{Error, Result},
    proto::{DllOverride, DllOverrideMode, Process},
    runner::{Runner, RunnerKind, shutdown_prefix},
    utils::environment::Environment,
    winebridge::WineBridgeClient,
    wrapper::Wrappers,
};

#[derive(Clone, Debug, Deserialize, Serialize, Config)]
#[config(version = 1)]
pub struct BottleState {
    pub(crate) id: Uuid,
    pub(crate) name: String,
    pub(crate) storage: PrefixStorage,
    #[serde(default)]
    pub(crate) programs: Vec<Program>,

    #[serde(flatten)]
    pub(crate) components: BottleComponents,
    #[serde(default)]
    pub(crate) dependencies: Vec<Dependency>,
    #[serde(default, skip_serializing_if = "Environment::is_empty")]
    pub(crate) environment: Environment,

    #[serde(flatten)]
    pub(crate) wrappers: Wrappers,
}

impl BottleState {
    pub fn kind(&self) -> BottleType {
        self.storage.kind()
    }
}

pub struct Bottle {
    pub(crate) state: BottleState,
    pub(crate) cx: Context,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteProgress {
    Stopping,
    Removing,
}

impl Bottle {
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        components: BottleComponents,
        dependencies: Vec<Dependency>,
        storage: PrefixStorage,
        context: Context,
    ) -> Result<Self> {
        let bottle = Self {
            state: BottleState {
                id,
                name,
                components,
                dependencies,
                storage,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                environment: Environment::default(),
            },
            cx: context,
        };
        bottle.save().await?;
        Ok(bottle)
    }

    pub(crate) fn from_state(state: BottleState, cx: Context) -> Self {
        Self { state, cx }
    }

    pub fn state(&self) -> &BottleState {
        &self.state
    }

    pub fn edit(self) -> BottleEdit {
        BottleEdit::new(self)
    }

    pub fn delete(self) -> crate::Operation<(), DeleteProgress> {
        let runtime = self.cx.clone();
        let cx = self.cx.clone();
        runtime.spawn(move |progress, cancellation| async move {
            progress.send_replace(Some(DeleteProgress::Stopping));
            self.stop().await?;
            if cancellation.is_cancelled() {
                return Err(Error::Cancelled);
            }
            progress.send_replace(Some(DeleteProgress::Removing));
            let path = cx.directories().bottle(self.id());
            cx.spawn_blocking(move || {
                std::fs::remove_dir_all(path)?;
                Ok(())
            })
            .await
        })
    }

    pub fn id(&self) -> Uuid {
        self.state.id
    }

    pub fn name(&self) -> &str {
        &self.state.name
    }

    pub fn components(&self) -> &BottleComponents {
        &self.state.components
    }

    pub fn runner(&self) -> &Component {
        self.state.components.runner()
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.state.dependencies
    }

    pub fn environment(&self) -> &Environment {
        &self.state.environment
    }

    pub async fn dll_overrides(&self) -> Result<Vec<DllOverride>> {
        self.with_bridge(|bridge| async move {
            match bridge.list_dll_overrides().await {
                Ok(overrides) => Ok(overrides),
                Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => {
                    Ok(Vec::new())
                }
                Err(error) => Err(error),
            }
        })
        .await
    }

    pub async fn set_dll_override(
        &self,
        dll: impl Into<String>,
        mode: DllOverrideMode,
    ) -> Result<()> {
        let dll = dll.into();
        if mode == DllOverrideMode::Unspecified {
            return Err(BottleError::DllOverrideModeRequired.into());
        }
        self.with_bridge(move |bridge| async move { bridge.set_dll_override(dll, mode).await })
            .await
    }

    pub async fn unset_dll_override(&self, dll: impl Into<String>) -> Result<()> {
        let dll = dll.into();
        self.with_bridge(move |bridge| async move {
            match bridge.delete_dll_override(dll).await {
                Err(Error::Status(status)) if status.code() == tonic::Code::NotFound => Ok(()),
                result => result,
            }
        })
        .await
    }

    pub fn wrappers(&self) -> &Wrappers {
        &self.state.wrappers
    }

    pub fn r#type(&self) -> BottleType {
        self.state.kind()
    }

    pub fn programs(&self) -> &[Program] {
        &self.state.programs
    }

    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.state.programs.iter().find(|program| program.id == id)
    }

    /// Launch a tracked program, starting WineBridge if it is not already running.
    pub async fn run(&self, id: Uuid) -> Result<u32> {
        let program = self
            .program(id)
            .cloned()
            .ok_or(BottleError::ProgramNotFound(id))?;
        self.with_bridge(move |bridge| async move {
            bridge
                .launch_process(
                    program.id,
                    program.executable,
                    program.args,
                    program.working_directory,
                    program.new_console,
                )
                .await
        })
        .await
    }

    /// List processes, starting WineBridge if it is not already running.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.with_bridge(|bridge| async move { bridge.list_processes().await })
            .await
    }

    /// Kill a tracked program by UUID, starting WineBridge if necessary.
    pub async fn kill(&self, id: Uuid) -> Result<()> {
        if self.program(id).is_none() {
            return Err(BottleError::ProgramNotFound(id).into());
        }
        self.with_bridge(move |bridge| async move { bridge.kill_process(id).await })
            .await
    }

    /// Stop WineBridge, wineserver, and prefix storage.
    pub fn stop(&self) -> Operation<(), ()> {
        let prefix_path = self.prefix_path();
        let runner = self.load_runner();
        let storage = self.state.storage.clone();
        let bottle_path = self.bottle_path();
        let cx = self.cx.clone();
        let runtime = self.cx.clone();
        runtime.spawn(move |_, _| async move {
            let mut first_error = None;
            match WineBridgeClient::try_connect(&prefix_path).await {
                Ok(Some(bridge)) => {
                    if let Err(error) = bridge.shutdown().await {
                        first_error.get_or_insert(error);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }

            let runner = match runner {
                Ok(runner) => Some(runner),
                Err(error) => {
                    first_error.get_or_insert(error);

                    None
                }
            };

            if let Some(runner) = runner.as_deref()
                && let Err(error) = shutdown_prefix(runner, &prefix_path).await
            {
                first_error.get_or_insert(error);
            }

            if let Err(error) = storage.stop(&bottle_path, &cx).await {
                first_error.get_or_insert(error);
            }

            first_error.map_or(Ok(()), Err)
        })
    }

    /// Standard-prefix effects completed before a metadata error are not rolled back.
    pub async fn install_component(&mut self, component: &Component) -> Result<()> {
        match component.kind() {
            ComponentKind::Runner { .. } => Err(BottleError::RunnerRequiresExplicitInstall.into()),
            ComponentKind::Winebridge => {
                if self.components().winebridge.id() == component.id() {
                    return Ok(());
                }
                self.update(async |bottle| {
                    bottle.stop().await?;
                    bottle.state.components.winebridge = component.clone();
                    Ok(())
                })
                .await
            }
            ComponentKind::Umu => {
                if self.runner().kind().runner_kind() != Some(RunnerKind::Proton) {
                    return Err(BottleError::WineRunnerWithUmu.into());
                }
                if self.components().umu.as_ref().map(Component::id) == Some(component.id()) {
                    return Ok(());
                }
                self.update(async |bottle| {
                    bottle.stop().await?;
                    bottle.state.components.umu = Some(component.clone());
                    Ok(())
                })
                .await
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

        let resources = component.prepare(self.cx.directories())?;
        let winebridge = self.components().winebridge.path().to_path_buf();
        self.update(async |bottle| {
            bottle.stop().await?;
            bottle
                .state
                .components
                .slot_mut(component.kind())?
                .take()
                .ok_or(BottleError::ComponentNotInstalled(id))?;

            let runner = bottle.load_runner()?;
            let bottle_path = bottle.bottle_path();
            let context = bottle.cx.clone();
            let BottleState {
                storage,
                environment,
                ..
            } = &mut bottle.state;
            storage
                .uninstall(
                    &bottle_path,
                    component.id(),
                    async |prefix, restore_files| {
                        crate::compatibility::installer::uninstall(
                            crate::compatibility::installer::InstallInputs {
                                prefix,
                                runner: runner.as_ref(),
                                winebridge: &winebridge,
                                environment,
                            },
                            &resources,
                            restore_files,
                            component.id(),
                        )
                        .await
                    },
                    &context,
                )
                .await
        })
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
        let resources = component.prepare(self.cx.directories())?;
        self.install_item(component.id(), replaced_id, resources, |config| {
            config.components.slot_mut(kind)?.replace(component.clone());
            Ok(())
        })
        .await
    }

    async fn install_item<F>(
        &mut self,
        item_id: Uuid,
        replaced_id: Option<Uuid>,
        resources: Vec<InstallResource>,
        update_config: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut BottleState) -> Result<()>,
    {
        self.update(async move |bottle| {
            bottle.stop().await?;
            update_config(&mut bottle.state)?;

            let runner = bottle.load_runner()?;
            let winebridge = bottle.components().winebridge.path().to_path_buf();
            let bottle_path = bottle.bottle_path();
            let context = bottle.cx.clone();
            let BottleState {
                storage,
                environment,
                ..
            } = &mut bottle.state;
            storage
                .install(
                    &bottle_path,
                    item_id,
                    replaced_id,
                    async |prefix| {
                        crate::compatibility::installer::execute(
                            &context,
                            crate::compatibility::installer::InstallInputs {
                                prefix,
                                runner: runner.as_ref(),
                                winebridge: &winebridge,
                                environment,
                            },
                            &resources,
                        )
                        .await
                    },
                    &context,
                )
                .await?;
            crate::compatibility::installer::replay_environment(environment, &resources);
            Ok(())
        })
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
        let resources = dependency.prepare(self.cx.directories())?;
        self.install_item(dependency.id(), None, resources, |config| {
            config.dependencies.push(dependency.clone());
            Ok(())
        })
        .await
    }

    pub async fn install_runner(
        &mut self,
        component: &Component,
        umu: Option<&Component>,
    ) -> Result<()> {
        BottleComponents::new(component, self.components().winebridge(), umu)?;
        if self.runner().id() == component.id()
            && self.components().umu().map(Component::id) == umu.map(Component::id)
        {
            return Ok(());
        }
        let installed = self
            .components()
            .into_iter()
            .map(Component::id)
            .chain(self.dependencies().iter().map(Dependency::id))
            .collect::<Vec<_>>();
        self.update(async |bottle| {
            bottle.stop().await?;
            bottle.state.components.runner = component.clone();
            bottle.state.components.umu = umu.cloned();

            let runner = bottle.load_runner()?;
            let context = bottle.cx.clone();
            bottle
                .state
                .storage
                .rebuild(
                    runner.as_ref(),
                    &component.id().to_string(),
                    &installed,
                    &context,
                )
                .await
        })
        .await
    }

    pub(crate) fn load_runner(&self) -> Result<Box<dyn Runner>> {
        let kind = self
            .state
            .components
            .runner()
            .kind()
            .runner_kind()
            .ok_or(BottleError::RunnerComponentRequired)?;
        crate::runner::load_runner(
            self.state.components.runner().path(),
            kind,
            self.state.components.umu().map(Component::path),
        )
    }

    async fn update<F, R>(&mut self, operation: F) -> Result<R>
    where
        F: for<'a> AsyncFnOnce(&'a mut Bottle) -> Result<R>,
    {
        let previous = self.state.clone();
        let value = match operation(self).await {
            Ok(value) => value,
            Err(error) => {
                self.state = previous;
                return Err(error);
            }
        };
        if let Err(error) = self.save().await {
            self.state = previous;
            return Err(error);
        }
        Ok(value)
    }

    pub(crate) fn bottle_path(&self) -> PathBuf {
        self.cx.directories().bottle(self.id())
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    async fn save(&self) -> Result<()> {
        let path = self.bottle_path().join("bottle.toml");
        let state = self.state.clone();
        self.cx
            .spawn_blocking(move || {
                next_config::save(path, &state)?;
                Ok(())
            })
            .await
    }

    async fn with_bridge<T, F, Fut>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(WineBridgeClient) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let runner = self.load_runner()?;
        let bottle_path = self.bottle_path();
        let prefix = self.prefix_path();
        let storage = self.state.storage.clone();
        let cx = self.cx.clone();
        let command = self.state.wrappers.apply(
            WineBridgeClient::command(
                runner.as_ref(),
                &prefix,
                self.components().winebridge().path(),
            )
            .envs(self.state.environment.iter()),
        );
        let operation: Operation<_, ()> = self.cx.spawn(move |_, _| async move {
            storage.prepare(&bottle_path, &cx).await?;
            work(WineBridgeClient::connect_or_spawn(&prefix, command).await?).await
        });
        operation.await
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
    #[cfg(unix)]
    async fn failed_update_does_not_publish_working_config() {
        use std::{fs, os::unix::fs::PermissionsExt};

        let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
        let runner_root = root.join("runner");
        fs::create_dir_all(runner_root.join("bin")).unwrap();
        for executable in ["wine", "wineserver"] {
            let path = runner_root.join("bin").join(executable);
            fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let context = Context::new(
            crate::Directories {
                data_dir: root.join("data"),
                runtime_dir: root.join("run"),
            },
            root.join("fvs2d"),
        )
        .unwrap();
        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            &runner_root,
        )
        .unwrap();
        let winebridge = Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
        let mut bottle = Bottle::from_state(
            BottleState {
                id: Uuid::new_v4(),
                name: "test".into(),
                storage: PrefixStorage::Standard,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                components: BottleComponents::new(&runner, &winebridge, None).unwrap(),
                dependencies: Vec::new(),
                environment: Environment::default(),
            },
            context,
        );

        let result = bottle
            .update(async |bottle| {
                bottle.state.storage = PrefixStorage::Virgo { layers: Vec::new() };
                bottle
                    .state
                    .environment
                    .insert("CHANGED".into(), "yes".into());
                Err::<(), _>(BottleError::InvalidProgram.into())
            })
            .await;

        assert!(result.is_err());
        assert!(matches!(bottle.state.storage, PrefixStorage::Standard));
        assert!(bottle.state.environment.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn runner_install_requires_the_explicit_runner_api_and_umu_selection() {
        let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
        let context = Context::new(
            crate::Directories {
                data_dir: root.join("data"),
                runtime_dir: root.join("run"),
            },
            root.join("fvs2d"),
        )
        .unwrap();
        let wine = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            root.join("wine"),
        )
        .unwrap();
        let proton = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Proton,
            },
            "proton",
            root.join("proton"),
        )
        .unwrap();
        let winebridge = Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
        let mut bottle = Bottle::from_state(
            BottleState {
                id: Uuid::new_v4(),
                name: "test".into(),
                storage: PrefixStorage::Standard,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                components: BottleComponents::new(&wine, &winebridge, None).unwrap(),
                dependencies: Vec::new(),
                environment: Environment::default(),
            },
            context,
        );

        assert!(matches!(
            bottle.install_component(&proton).await,
            Err(crate::error::Error::Bottle(
                BottleError::RunnerRequiresExplicitInstall
            ))
        ));
        assert!(matches!(
            bottle.install_runner(&proton, None).await,
            Err(crate::error::Error::Bottle(
                BottleError::ProtonRunnerWithoutUmu
            ))
        ));
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
