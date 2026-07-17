use std::{fs, path::Path};

use fvs_rs::{Layer, Repository, UnmountMode};
use uuid::Uuid;

use crate::{
    compatibility::installer::cached_layer,
    error::{Error, Result},
    runner::{Runner, initialize_and_shutdown_prefix},
};

use super::{
    bottle::{Bottle, PrefixStorage},
    error::BottleError,
    manager::{BottleManager, fvs},
};

const BLOCK_SIZE: u32 = 1024 * 1024;

impl BottleManager {
    pub(super) async fn ensure_base(&self, runner: &dyn Runner) -> Result<Layer> {
        let client = fvs().await?;
        let root = crate::utils::directories::expect()
            .data_dir()
            .join("virgo/base");
        let repository_path = root.join("prefix");
        if repository_path.join(".fvs2").is_dir() {
            let repository = client.new_repository(&repository_path, 0).await?;
            let commit = client
                .list_commits(&repository)
                .await?
                .into_iter()
                .next()
                .ok_or(BottleError::EmptyBase)?;
            return Ok(Layer::new(&repository, Some(&commit)));
        }
        if repository_path.exists() && repository_path.read_dir()?.next().is_some() {
            return Err(BottleError::DirtyBase(repository_path).into());
        }

        fs::create_dir_all(&repository_path)?;
        if let Err(error) = initialize_and_shutdown_prefix(runner, &repository_path) {
            let _ = fs::remove_dir_all(&root);
            return Err(error);
        }

        let committed = async {
            let repository = client.new_repository(&repository_path, BLOCK_SIZE).await?;
            let commit = client.commit(&repository, "Virgo base".into()).await?;
            Ok(Layer::new(&repository, Some(&commit)))
        }
        .await;
        if committed.is_err() {
            let _ = fs::remove_dir_all(root);
        }
        committed
    }

    pub(super) async fn ensure_adapter(
        &self,
        runner: &dyn Runner,
        runner_key: &str,
        base: &Layer,
    ) -> Result<Layer> {
        let client = fvs().await?;
        let destination = crate::utils::directories::expect()
            .data_dir()
            .join("virgo/adapters")
            .join(runner_key);
        if destination.join(".fvs2").is_dir() {
            let repository = client.new_repository(&destination, 0).await?;
            let commit = client
                .list_commits(&repository)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| BottleError::MissingCommit {
                    repository: destination.clone(),
                    state: "HEAD".into(),
                })?;
            return Ok(Layer::new(&repository, Some(&commit)));
        }

        let stage = crate::utils::directories::expect()
            .data_dir()
            .join("virgo/.staging")
            .join(Uuid::new_v4().to_string());
        let upper = stage.join("upper");
        let mountpoint = stage.join("prefix");
        fs::create_dir_all(&upper)?;
        fs::create_dir_all(&mountpoint)?;

        let build = async {
            let mount = client
                .mount(&mountpoint, vec![base.clone()], Some(&upper))
                .await?;
            let initialized = initialize_and_shutdown_prefix(runner, &mountpoint);
            let unmounted = client.unmount(&mount, UnmountMode::Normal).await;
            initialized?;
            unmounted?;

            let repository = client.new_repository(&upper, BLOCK_SIZE).await?;
            let commit = client
                .commit(&repository, format!("Runner adapter {runner_key}"))
                .await?;
            fs::create_dir_all(destination.parent().expect("adapter path has a parent"))?;
            fs::rename(&upper, &destination)?;
            Ok::<_, Error>(commit)
        }
        .await;
        let _ = fs::remove_dir_all(stage);

        let commit = build?;
        let repository = Repository {
            repository_path: destination.display().to_string(),
            block_size: BLOCK_SIZE,
        };
        Ok(Layer::new(&repository, Some(&commit)))
    }
}

impl Bottle {
    pub(crate) async fn prepare_virgo_layers(&mut self) -> Result<()> {
        if matches!(&self.storage, PrefixStorage::Virgo { layers } if layers.is_empty()) {
            self.refresh_virgo_layers().await?;
            self.save()?;
        }
        Ok(())
    }

    pub(crate) async fn refresh_virgo_layers(&mut self) -> Result<()> {
        let PrefixStorage::Virgo { layers } = &self.storage else {
            return Ok(());
        };
        let mut refreshed = if layers.len() >= 2 {
            layers[..2].to_vec()
        } else {
            let runner = self.load_runner()?;
            let manager = BottleManager;
            let base = manager.ensure_base(runner.as_ref()).await?;
            let adapter = manager
                .ensure_adapter(runner.as_ref(), &self.runner().id().to_string(), &base)
                .await?;
            vec![base, adapter]
        };

        for component in &self.components {
            refreshed.push(cached_layer(component.id()).await?);
        }
        for dependency in &self.dependencies {
            refreshed.push(cached_layer(dependency.id()).await?);
        }
        self.storage = PrefixStorage::Virgo { layers: refreshed };
        Ok(())
    }
}

pub(super) fn ensure_empty_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    if path.read_dir()?.next().is_some() {
        return Err(BottleError::DirtyMountpoint(path.to_path_buf()).into());
    }
    Ok(())
}
