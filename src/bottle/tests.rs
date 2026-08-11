use crate::{
    Context, Directories,
    addons::{Requirement, Slot},
    bottle::{BottleManager, Storage, error::BottleError},
    error::Error,
};
fn test_directories() -> Directories {
    let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
    Directories::from_path(root).unwrap()
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
        let manager = BottleManager::load(
            Context::for_test(
                directories.clone(),
                Some(directories.data_dir().join("fvs2d")),
            )
            .await
            .unwrap(),
        )
        .await
        .unwrap();

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
        .await
        .unwrap();
        let runner = context
            .addons()
            .components()
            .into_iter()
            .find(|addon| addon.slot() == Slot::Runner)
            .unwrap();
        let manager = BottleManager::new(context);

        let error = match manager.create("test", Storage::Standard, &runner).await {
            Ok(_) => panic!("creation should fail before mutation"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            Error::Bottle(BottleError::RequiresAddon {
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
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}
