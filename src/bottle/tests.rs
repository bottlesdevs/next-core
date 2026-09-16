#[cfg(feature = "fvs")]
use crate::environment::VirgoManager;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Mutex, watch};

use crate::environment::Environment;
use crate::{
    Context, Directories, EnvironmentError, PrefixBackend,
    addons::{AddonError, CatalogError, Requirement, Slot},
    bottle::{Bottle, BottleManager},
    error::Error,
};
fn test_directories() -> Directories {
    let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
    Directories::from_path(root).unwrap()
}

async fn deleted_bottle() -> (Bottle, Directories) {
    let directories = test_directories();
    let context = Context::for_test(directories.clone()).await.unwrap();
    let (published, _) = watch::channel(None);
    let bottle = Bottle(Arc::new(Environment {
        published,
        control: Mutex::new(()),
        root: directories.bottle(uuid::Uuid::new_v4()),
        #[cfg(feature = "fvs")]
        virgo: Arc::new(VirgoManager::new(context.clone())),
        context,
    }));
    (bottle, directories)
}

#[test]
fn bottle_edit_cancels_while_waiting_for_write_lock() {
    futures_lite::future::block_on(async {
        let (bottle, directories) = deleted_bottle().await;
        let write = bottle.0.control.lock().await;
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        let mut update = Box::pin(bottle.edit(move |_| {
            work_ran.store(true, Ordering::Relaxed);
            Ok(())
        }));
        let cancellation = update.cancellation_token();

        assert!(futures_lite::future::poll_once(&mut update).await.is_none());
        cancellation.cancel();

        assert!(matches!(
            futures_lite::future::poll_once(&mut update).await,
            Some(Err(Error::Cancelled))
        ));
        assert!(!ran.load(Ordering::Relaxed));
        drop(write);
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}

#[test]
fn bottle_edit_rechecks_cancellation_when_lock_becomes_available() {
    futures_lite::future::block_on(async {
        let (bottle, directories) = deleted_bottle().await;
        let write = bottle.0.control.lock().await;
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        let mut update = Box::pin(bottle.edit(move |_| {
            work_ran.store(true, Ordering::Relaxed);
            Ok(())
        }));
        let cancellation = update.cancellation_token();

        assert!(futures_lite::future::poll_once(&mut update).await.is_none());
        cancellation.cancel();
        drop(write);

        assert!(matches!(update.await, Err(Error::Cancelled)));
        assert!(!ran.load(Ordering::Relaxed));
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}

#[test]
fn load_skips_corrupt_bottles() {
    futures_lite::future::block_on(async {
        let directories = test_directories();
        let id = uuid::Uuid::new_v4();
        std::fs::create_dir_all(directories.bottle(id)).unwrap();
        std::fs::write(
            directories.bottle(id).join("bottle.toml"),
            "not valid toml =",
        )
        .unwrap();
        let context = Context::for_test(directories.clone()).await.unwrap();
        #[cfg(feature = "fvs")]
        let virgo = Arc::new(VirgoManager::new(context.clone()));
        let manager = BottleManager::load(
            context,
            #[cfg(feature = "fvs")]
            virgo,
        )
        .await
        .unwrap();

        assert!(manager.list().is_empty());
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}

#[test]
fn create_reports_missing_runtime_requirements_before_creating_files() {
    futures_lite::future::block_on(async {
        let directories = test_directories();
        let runner_path = directories.data_dir().join("proton-test.tar");
        let mut archive =
            smol_tar::TarWriter::new(async_fs::File::create(&runner_path).await.unwrap());
        archive
            .write(
                smol_tar::TarRegularFile::new("proton-test/proton", 0, &[][..])
                    .with_mode(0o755)
                    .into(),
            )
            .await
            .unwrap();
        archive.finish().await.unwrap();
        drop(archive);
        let context = Context::for_test(directories.clone()).await.unwrap();
        let addons = context.addons().clone();
        let runner = addons
            .import_component(&runner_path, Slot::Runner, "Proton", "proton-test")
            .await
            .unwrap();
        let runner_id = runner.id();
        for slot in [Slot::WineBridge, Slot::Umu] {
            assert!(matches!(
                addons
                    .import_component(&runner_path, slot, "Invalid runtime", "99.0.0")
                    .await,
                Err(Error::Addon(AddonError::InvalidComponent(_)))
            ));
            assert!(addons.components().iter().all(|addon| addon.slot() != slot));
        }
        assert_eq!(
            runner.path(&directories),
            directories
                .component_releases()
                .join(runner_id.to_string())
                .join("payload")
        );
        assert!(
            directories
                .component_releases()
                .join(runner_id.to_string())
                .join("release.toml")
                .is_file()
        );
        assert!(!directories.components().join("index.toml").exists());
        let unknown = uuid::Uuid::new_v4();
        assert!(matches!(
            addons.fetch_component(unknown).await,
            Err(Error::Addon(AddonError::Catalog(CatalogError::NotFound(id)))) if id == unknown
        ));
        assert!(matches!(
            addons.remove_component(unknown).await,
            Err(Error::Addon(AddonError::NotFound(id))) if id == unknown
        ));
        #[cfg(feature = "fvs")]
        let virgo = Arc::new(VirgoManager::new(context.clone()));
        let manager = BottleManager::new(
            context,
            #[cfg(feature = "fvs")]
            virgo,
        );

        let winebridge = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::new_v4(),
            "name": "WineBridge",
            "version": "1.0.0",
            "slot": "winebridge",
            "resources": [{"path": "", "steps": []}],
        }))
        .unwrap();
        let error = match manager
            .create(
                "test",
                PrefixBackend::Standard,
                runner.as_ref().clone(),
                winebridge,
                None,
            )
            .await
        {
            Ok(_) => panic!("creation should fail before mutation"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::Environment(EnvironmentError::RequiresAddon {
                required_by: Some(id),
                requirements,
            }) if id == runner_id && requirements == vec![Requirement::Slot(Slot::Umu)]
        ));
        assert!(manager.list().is_empty());
        assert!(
            std::fs::read_dir(directories.bottles())
                .unwrap()
                .next()
                .is_none()
        );
        let reloaded = Context::for_test(directories.clone()).await.unwrap();
        let reloaded_addons = reloaded.addons();
        assert_eq!(
            reloaded_addons.component(runner_id).unwrap().id(),
            runner_id
        );
        reloaded_addons.remove_component(runner_id).await.unwrap();
        assert!(reloaded_addons.component(runner_id).is_none());
        assert!(
            !directories
                .component_releases()
                .join(runner_id.to_string())
                .exists()
        );
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}
