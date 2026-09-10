use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tokio::sync::{Mutex, watch};
use tokio_util::sync::CancellationToken;

use super::state::BottleInner;
use crate::{
    Context, Directories, EnvironmentError, Storage,
    addons::{AddonError, Addons, CatalogError, Requirement, Slot},
    bottle::{Bottle, BottleManager},
    error::Error,
};
fn test_directories() -> Directories {
    let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
    Directories::from_path(root).unwrap()
}

async fn deleted_bottle() -> (Bottle, Directories) {
    let directories = test_directories();
    let context = Context::for_test(
        directories.clone(),
        Some(directories.data_dir().join("fvs2d")),
    )
    .unwrap();
    let addons = Addons::load(context.clone(), None, None).await.unwrap();
    let (published, _) = watch::channel(None);
    let bottle = Bottle(Arc::new(BottleInner {
        published,
        environment: Mutex::new(None),
        id: uuid::Uuid::new_v4(),
        cx: context,
        addons,
    }));
    (bottle, directories)
}

#[test]
fn bottle_update_cancels_while_waiting_for_write_lock() {
    futures_lite::future::block_on(async {
        let (bottle, directories) = deleted_bottle().await;
        let write = bottle.0.environment.lock().await;
        let cancellation = CancellationToken::new();
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        let mut update = Box::pin(bottle.update(Some(&cancellation), async move |_, _, _| {
            work_ran.store(true, Ordering::Relaxed);
            Ok(())
        }));

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
fn bottle_update_rechecks_cancellation_when_lock_becomes_available() {
    futures_lite::future::block_on(async {
        let (bottle, directories) = deleted_bottle().await;
        let write = bottle.0.environment.lock().await;
        let cancellation = CancellationToken::new();
        let ran = Arc::new(AtomicBool::new(false));
        let work_ran = ran.clone();
        let mut update = Box::pin(bottle.update(Some(&cancellation), async move |_, _, _| {
            work_ran.store(true, Ordering::Relaxed);
            Ok(())
        }));

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
        let context = Context::for_test(
            directories.clone(),
            Some(directories.data_dir().join("fvs2d")),
        )
        .unwrap();
        let addons = Addons::load(context.clone(), None, None).await.unwrap();
        let manager = BottleManager::load(context, addons).await.unwrap();

        assert!(manager.list().is_empty());
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}

#[test]
fn create_reports_all_missing_runtime_addons_before_creating_files() {
    futures_lite::future::block_on(async {
        let directories = test_directories();
        let runner_path = directories.components().join("runner/proton-test");
        std::fs::create_dir_all(&runner_path).unwrap();
        std::fs::write(runner_path.join("proton"), []).unwrap();
        let context = Context::for_test(
            directories.clone(),
            Some(directories.data_dir().join("fvs2d")),
        )
        .unwrap();
        let addons = Addons::load(context.clone(), None, None).await.unwrap();
        let runner = addons
            .components()
            .into_iter()
            .find(|addon| addon.slot() == Slot::Runner)
            .unwrap();
        assert_eq!(runner.path(&directories), runner_path);
        let runner_id = runner.id();
        assert!(directories.components().join("index.toml").is_file());
        assert!(
            !std::fs::read_to_string(directories.components().join("index.toml"))
                .unwrap()
                .contains("path =")
        );
        assert!(!runner_path.join(".addon.toml").exists());
        let unknown = uuid::Uuid::new_v4();
        assert!(matches!(
            addons.fetch_component(unknown).await,
            Err(Error::Addon(AddonError::Catalog(CatalogError::NotFound(id)))) if id == unknown
        ));
        assert!(matches!(
            addons.remove_component(unknown).await,
            Err(Error::Addon(AddonError::NotFound(id))) if id == unknown
        ));
        let manager = BottleManager::new(context, addons);

        let error = match manager.create("test", Storage::Standard, runner_id).await {
            Ok(_) => panic!("creation should fail before mutation"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::Environment(EnvironmentError::RequiresAddon {
                required_by: None,
                requirements,
            }) if requirements == vec![
                Requirement::Slot(Slot::WineBridge),
                Requirement::Slot(Slot::Umu),
            ]
        ));
        assert!(manager.list().is_empty());
        assert!(
            std::fs::read_dir(directories.bottles())
                .unwrap()
                .next()
                .is_none()
        );
        let reloaded = Context::for_test(
            directories.clone(),
            Some(directories.data_dir().join("fvs2d")),
        )
        .unwrap();
        let reloaded_addons = Addons::load(reloaded, None, None).await.unwrap();
        assert_eq!(
            reloaded_addons.component(runner_id).unwrap().id(),
            runner_id
        );
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}
