use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::{NonNilUuid, Uuid};

use crate::{
    error::Result,
    runner::{Proton, Runner, RunnerError, RunnerKind, Wine, detect_runner_kind},
};

use super::{
    catalog::InternalRole,
    installer::{InstallResource, InstallStep},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Slot {
    Dxvk,
    Vkd3d,
    Nvapi,
    LatencyFlex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Downloaded,
    Downloadable,
    Unsupported,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Resource {
    source: PathBuf,
    steps: Vec<InstallStep>,
}

impl Resource {
    pub(crate) fn new(source: PathBuf, steps: Vec<InstallStep>) -> Self {
        Self { source, steps }
    }

    pub(crate) fn install(&self) -> InstallResource {
        InstallResource {
            source: self.source.clone(),
            steps: self.steps.clone(),
        }
    }

    pub(crate) fn source(&self) -> &Path {
        &self.source
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RunnerComponent {
    id: NonNilUuid,
    name: String,
    version: String,
    flavour: RunnerKind,
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    umu: Option<InternalComponent>,
    supported: bool,
}

impl RunnerComponent {
    pub(crate) fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        flavour: RunnerKind,
        path: Option<PathBuf>,
        supported: bool,
    ) -> Self {
        Self {
            id,
            name,
            version,
            flavour,
            path,
            umu: None,
            supported,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(flavour: RunnerKind, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            String::from("test runner"),
            String::from("test"),
            flavour,
            Some(crate::utils::absolute_path(path.into())?),
            true,
        ))
    }

    pub fn id(&self) -> Uuid {
        self.id.get()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn flavour(&self) -> RunnerKind {
        self.flavour
    }
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn availability(&self) -> Availability {
        if self.path.is_some() {
            Availability::Downloaded
        } else if self.supported {
            Availability::Downloadable
        } else {
            Availability::Unsupported
        }
    }

    pub(crate) fn installed_path(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .ok_or_else(|| super::AddonError::ItemNotDownloaded(self.id()).into())
    }

    pub(crate) fn pair_umu(&mut self, umu: Option<InternalComponent>) {
        self.umu = match self.flavour {
            RunnerKind::Wine => None,
            RunnerKind::Proton => umu,
        };
    }

    pub(crate) fn umu_path(&self) -> Option<&Path> {
        self.umu.as_ref().map(InternalComponent::path)
    }

    pub(crate) async fn load(&self) -> Result<Box<dyn Runner>> {
        let path = self.installed_path()?;
        if detect_runner_kind(path).await? != self.flavour {
            return Err(RunnerError::RunnerNotFound(path.to_path_buf()).into());
        }
        match self.flavour {
            RunnerKind::Wine => Ok(Box::new(Wine::new(path.join("bin/wine")))),
            RunnerKind::Proton => {
                let umu = self
                    .umu_path()
                    .ok_or(RunnerError::UmuExecutableMissing)?
                    .join("umu-run");
                if !async_fs::metadata(&umu)
                    .await
                    .is_ok_and(|entry| entry.is_file())
                {
                    return Err(RunnerError::RunnerExecutableNotFound(umu).into());
                }
                Ok(Box::new(Proton::new(path, umu)))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Addon {
    id: NonNilUuid,
    name: String,
    version: String,
    slot: Option<Slot>,
    resources: Vec<Resource>,
    supported: bool,
}

impl Addon {
    pub(crate) fn new(
        id: NonNilUuid,
        name: String,
        version: String,
        slot: Option<Slot>,
        resources: Vec<Resource>,
        supported: bool,
    ) -> Self {
        Self {
            id,
            name,
            version,
            slot,
            resources,
            supported,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(slot: Option<Slot>, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            String::from("test addon"),
            String::from("test"),
            slot,
            vec![Resource::new(
                crate::utils::absolute_path(path.into())?,
                Vec::new(),
            )],
            true,
        ))
    }

    pub fn id(&self) -> Uuid {
        self.id.get()
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn slot(&self) -> Option<Slot> {
        self.slot
    }

    pub fn availability(&self) -> Availability {
        if !self.resources.is_empty() {
            Availability::Downloaded
        } else if self.supported {
            Availability::Downloadable
        } else {
            Availability::Unsupported
        }
    }

    pub(crate) fn prepare(&self) -> Result<Vec<InstallResource>> {
        if self.resources.is_empty() {
            return Err(super::AddonError::ItemNotDownloaded(self.id()).into());
        }
        Ok(self.resources.iter().map(Resource::install).collect())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct InternalComponent {
    id: NonNilUuid,
    role: InternalRole,
    path: PathBuf,
}

impl InternalComponent {
    pub(crate) fn new(id: NonNilUuid, role: InternalRole, path: PathBuf) -> Self {
        Self { id, role, path }
    }

    #[cfg(test)]
    pub(crate) fn for_test(role: InternalRole, path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self::new(
            NonNilUuid::new(Uuid::new_v4()).expect("v4 UUID is non-nil"),
            role,
            crate::utils::absolute_path(path.into())?,
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn proton_load_uses_its_paired_umu() {
        futures_lite::future::block_on(async {
            let root = std::env::temp_dir().join(format!("bottles-next-runner-{}", Uuid::new_v4()));
            let proton = root.join("proton");
            let umu = root.join("umu");
            fs::create_dir_all(&proton).unwrap();
            fs::create_dir_all(&umu).unwrap();
            fs::write(proton.join("proton"), []).unwrap();
            fs::write(umu.join("umu-run"), []).unwrap();
            let mut runner = RunnerComponent::new(
                NonNilUuid::new(Uuid::new_v4()).unwrap(),
                "Proton".into(),
                "test".into(),
                RunnerKind::Proton,
                Some(proton),
                true,
            );

            assert!(matches!(
                runner.load().await,
                Err(crate::error::Error::Runner(
                    RunnerError::UmuExecutableMissing
                ))
            ));
            runner.pair_umu(Some(
                InternalComponent::for_test(InternalRole::Umu, umu).unwrap(),
            ));
            runner.load().await.unwrap();
            fs::remove_dir_all(root).unwrap();
        });
    }
}
