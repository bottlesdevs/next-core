//! Persisted bottle state and the shared bottle handle.

use std::{
    collections::HashMap,
    ops::AsyncFnOnce,
    path::PathBuf,
    sync::{Arc, Weak},
};

use next_config::Config;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, watch};
use uuid::Uuid;

use super::{PrefixStorage, edit::BottleEdit, error::BottleError};
use crate::{
    Context,
    compatibility::{
        components::{Component, catalog::ComponentKind},
        dependencies::Dependency,
    },
    error::Result,
    runner::RunnerKind,
    utils::environment::Environment,
    wrapper::Wrappers,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize, Config)]
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
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn components(&self) -> &BottleComponents {
        &self.components
    }

    pub fn runner(&self) -> &RunnerSelection {
        self.components.runner()
    }

    pub fn dependencies(&self) -> &[Dependency] {
        &self.dependencies
    }

    pub fn environment(&self) -> &Environment {
        &self.environment
    }

    pub fn wrappers(&self) -> &Wrappers {
        &self.wrappers
    }

    pub fn programs(&self) -> &[Program] {
        &self.programs
    }

    pub fn program(&self, id: Uuid) -> Option<&Program> {
        self.programs.iter().find(|program| program.id == id)
    }

    pub fn kind(&self) -> BottleType {
        self.storage.kind()
    }
}

pub(crate) type BottleCache = Mutex<HashMap<Uuid, Weak<BottleInner>>>;

pub(crate) struct BottleInner {
    pub(crate) published: watch::Sender<Option<Arc<BottleState>>>,
    pub(crate) write: Mutex<()>,
    pub(crate) id: Uuid,
    pub(crate) cx: Context,
}

#[derive(Clone)]
pub struct Bottle(pub(crate) Arc<BottleInner>);

impl Bottle {
    pub(crate) async fn new(
        id: Uuid,
        name: String,
        components: BottleComponents,
        dependencies: Vec<Dependency>,
        storage: PrefixStorage,
        context: Context,
    ) -> Result<Self> {
        let bottle = Self::from_state(
            BottleState {
                id,
                name,
                components,
                dependencies,
                storage,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                environment: Environment::default(),
            },
            context,
        );
        bottle.save().await?;
        Ok(bottle)
    }

    pub(crate) fn from_state(state: BottleState, cx: Context) -> Self {
        let id = state.id;
        let (published, _) = watch::channel(Some(Arc::new(state)));
        Self::from_inner(Arc::new(BottleInner {
            id,
            published,
            write: Mutex::new(()),
            cx,
        }))
    }

    pub(crate) fn from_inner(inner: Arc<BottleInner>) -> Self {
        Self(inner)
    }

    pub fn state(&self) -> Result<Arc<BottleState>> {
        self.0
            .published
            .borrow()
            .clone()
            .ok_or_else(|| BottleError::Deleted(self.0.id).into())
    }

    pub fn edit(&self) -> BottleEdit {
        BottleEdit::new(self.clone())
    }

    pub(crate) fn ensure_exists(&self) -> Result<()> {
        if self.is_deleted() {
            Err(BottleError::Deleted(self.0.id).into())
        } else {
            Ok(())
        }
    }

    pub(crate) fn is_deleted(&self) -> bool {
        self.0.published.borrow().is_none()
    }

    pub(crate) fn mark_deleted(&self) {
        self.0.published.send_replace(None);
    }

    pub(super) async fn update<F, R>(&self, operation: F) -> Result<R>
    where
        F: for<'a> AsyncFnOnce(&'a mut BottleState, Context) -> Result<R>,
    {
        let _write = self.0.write.lock().await;
        let mut draft = self.state()?.as_ref().clone();
        let value = operation(&mut draft, self.0.cx.clone()).await?;
        Self::save_state(&draft, &self.0.cx).await?;
        self.publish(draft);
        Ok(value)
    }

    pub(crate) fn publish(&self, state: BottleState) {
        let next = Arc::new(state);
        self.0.published.send_if_modified(|published| {
            if published.as_deref() == Some(next.as_ref()) {
                false
            } else {
                *published = Some(next);
                true
            }
        });
    }

    pub(crate) fn bottle_path(&self) -> PathBuf {
        self.0.cx.directories().bottle(self.0.id)
    }

    pub(crate) fn prefix_path(&self) -> PathBuf {
        self.bottle_path().join("prefix")
    }

    async fn save(&self) -> Result<()> {
        let state = self.state()?;
        Self::save_state(&state, &self.0.cx).await
    }

    async fn save_state(state: &BottleState, cx: &Context) -> Result<()> {
        let path = cx.directories().bottle(state.id).join("bottle.toml");
        let state = state.clone();
        cx.spawn_blocking(move || {
            next_config::save(path, &state)?;
            Ok(())
        })
        .await
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum RunnerSelection {
    Wine { runner: Component },
    Proton { runner: Component, umu: Component },
}

impl RunnerSelection {
    pub fn wine(runner: Component) -> Result<Self> {
        let selection = Self::Wine { runner };
        selection.validate()?;
        Ok(selection)
    }

    pub fn proton(runner: Component, umu: Component) -> Result<Self> {
        let selection = Self::Proton { runner, umu };
        selection.validate()?;
        Ok(selection)
    }

    pub fn runner(&self) -> &Component {
        match self {
            Self::Wine { runner } | Self::Proton { runner, .. } => runner,
        }
    }

    pub fn umu(&self) -> Option<&Component> {
        match self {
            Self::Wine { .. } => None,
            Self::Proton { umu, .. } => Some(umu),
        }
    }

    pub fn kind(&self) -> RunnerKind {
        match self {
            Self::Wine { .. } => RunnerKind::Wine,
            Self::Proton { .. } => RunnerKind::Proton,
        }
    }

    pub(super) fn validate(&self) -> Result<()> {
        if self.runner().kind().runner_kind() != Some(self.kind()) {
            return Err(BottleError::RunnerComponentRequired.into());
        }
        if self
            .umu()
            .is_some_and(|component| component.kind() != ComponentKind::Umu)
        {
            return Err(BottleError::InvalidUmuComponent.into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct BottleComponents {
    pub(crate) runner: RunnerSelection,
    pub(crate) winebridge: Component,
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
    pub fn new(runner: RunnerSelection, winebridge: &Component) -> Result<Self> {
        runner.validate()?;
        if winebridge.kind() != ComponentKind::Winebridge {
            return Err(BottleError::WinebridgeComponentRequired.into());
        }

        Ok(Self {
            runner,
            winebridge: winebridge.clone(),
            dxvk: None,
            vkd3d: None,
            nvapi: None,
            latency_flex: None,
        })
    }

    pub fn runner(&self) -> &RunnerSelection {
        &self.runner
    }

    pub fn winebridge(&self) -> &Component {
        &self.winebridge
    }

    pub fn umu(&self) -> Option<&Component> {
        self.runner.umu()
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

    pub(super) fn slot(&self, kind: ComponentKind) -> Result<Option<&Component>> {
        match kind {
            ComponentKind::Dxvk => Ok(self.dxvk.as_ref()),
            ComponentKind::Vkd3d => Ok(self.vkd3d.as_ref()),
            ComponentKind::Nvapi => Ok(self.nvapi.as_ref()),
            ComponentKind::LatencyFlex => Ok(self.latency_flex.as_ref()),
            _ => Err(BottleError::InvalidPrefixComponent.into()),
        }
    }

    pub(super) fn slot_mut(&mut self, kind: ComponentKind) -> Result<&mut Option<Component>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg(unix)]
    async fn updates_are_serialized_and_failed_updates_are_rolled_back() {
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
            crate::Directories::from_path(root.join("data")).unwrap(),
            root.join("fvs2d"),
        )
        .unwrap();
        let id = Uuid::new_v4();
        fs::create_dir_all(context.directories().bottle(id)).unwrap();
        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            &runner_root,
        )
        .unwrap();
        let winebridge = Component::new(ComponentKind::Winebridge, "bridge", "/bridge").unwrap();
        let bottle = Bottle::from_state(
            BottleState {
                id,
                name: "test".into(),
                storage: PrefixStorage::Standard,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                components: BottleComponents::new(
                    RunnerSelection::wine(runner.clone()).unwrap(),
                    &winebridge,
                )
                .unwrap(),
                dependencies: Vec::new(),
                environment: Environment::default(),
            },
            context,
        );
        bottle.save().await.unwrap();

        let result = bottle
            .update(async |state, _| {
                state.storage = PrefixStorage::Virgo { layers: Vec::new() };
                state.environment.insert("CHANGED".into(), "yes".into());
                Err::<(), _>(BottleError::InvalidProgram.into())
            })
            .await;

        assert!(result.is_err());
        let state = bottle.state().unwrap();
        assert!(matches!(state.storage, PrefixStorage::Standard));
        assert!(state.environment.is_empty());

        let first_bottle = bottle.clone();
        let first = async move {
            first_bottle
                .update(async |state, _| {
                    state.environment.insert("FIRST".into(), "yes".into());
                    tokio::task::yield_now().await;
                    Ok(())
                })
                .await
        };
        let second_bottle = bottle.clone();
        let second = async move {
            second_bottle
                .update(async |state, _| {
                    state.environment.insert("SECOND".into(), "yes".into());
                    Ok(())
                })
                .await
        };
        let (first, second) = tokio::join!(first, second);
        first.unwrap();
        second.unwrap();

        let state = bottle.state().unwrap();
        assert_eq!(state.environment.get("FIRST"), Some("yes"));
        assert_eq!(state.environment.get("SECOND"), Some("yes"));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn runner_install_requires_the_explicit_runner_api_and_umu_selection() {
        let root = std::env::temp_dir().join(format!("bottles-next-{}", Uuid::new_v4()));
        let context = Context::new(
            crate::Directories::from_path(root.join("data")).unwrap(),
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
        let bottle = Bottle::from_state(
            BottleState {
                id: Uuid::new_v4(),
                name: "test".into(),
                storage: PrefixStorage::Standard,
                programs: Vec::new(),
                wrappers: Wrappers::default(),
                components: BottleComponents::new(
                    RunnerSelection::wine(wine.clone()).unwrap(),
                    &winebridge,
                )
                .unwrap(),
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
        let mut components =
            BottleComponents::new(RunnerSelection::wine(runner.clone()).unwrap(), &winebridge)
                .unwrap();

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
