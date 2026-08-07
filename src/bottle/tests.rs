use crate::{
    Context, Directories,
    addons::{Addon, RunnerComponent, Slot, catalog::InternalRole, item::InternalComponent},
    bottle::{BottleManager, BottleState, Storage},
    prefix::Prefix,
    runner::RunnerKind,
    utils::environment::Environment,
    wrapper::Wrappers,
};
fn test_directories() -> Directories {
    let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
    Directories::from_path(root).unwrap()
}

fn state(directories: &Directories, id: uuid::Uuid, name: &str) -> BottleState {
    BottleState {
        id,
        name: name.into(),
        storage: Prefix::Standard,
        programs: Vec::new(),
        runner: RunnerComponent::for_test(RunnerKind::Wine, directories.data_dir().join("runner"))
            .unwrap(),
        winebridge: InternalComponent::for_test(
            InternalRole::Winebridge,
            directories.data_dir().join("winebridge"),
        )
        .unwrap(),
        addons: Vec::new(),
        environment: Environment::default(),
        wrappers: Wrappers::default(),
    }
}

#[test]
fn bottle_managers_are_scoped_to_their_context_roots() {
    futures_lite::future::block_on(async {
        let id = uuid::Uuid::new_v4();
        let left = test_directories();
        let right = test_directories();

        for (directories, name) in [(&left, "left"), (&right, "right")] {
            std::fs::create_dir_all(directories.bottle(id)).unwrap();
            next_config::save(
                directories.bottle(id).join("bottle.toml"),
                &state(directories, id, name),
            )
            .await
            .unwrap();
        }

        for (directories, name) in [(&left, "left"), (&right, "right")] {
            let manager = BottleManager::new(
                Context::for_test(
                    directories.clone(),
                    Some(directories.data_dir().join("fvs2d")),
                )
                .await
                .unwrap(),
            );
            assert_eq!(
                manager.open(id).await.unwrap().state().unwrap().name(),
                name
            );
        }

        std::fs::remove_dir_all(left.data_dir()).unwrap();
        std::fs::remove_dir_all(right.data_dir()).unwrap();
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
fn addons_round_trip_and_slots_replace_without_disturbing_stacking_addons() {
    futures_lite::future::block_on(async {
        let directories = test_directories();
        let id = uuid::Uuid::new_v4();
        let path = directories.bottle(id).join("bottle.toml");
        std::fs::create_dir_all(directories.bottle(id)).unwrap();
        let mut bottle = state(&directories, id, "addons");
        let first_dxvk =
            Addon::for_test(Some(Slot::Dxvk), directories.data_dir().join("dxvk-1")).unwrap();
        let second_dxvk =
            Addon::for_test(Some(Slot::Dxvk), directories.data_dir().join("dxvk-2")).unwrap();
        let stacking = Addon::for_test(None, directories.data_dir().join("vcrun")).unwrap();

        bottle.put_addon(first_dxvk.clone());
        bottle.put_addon(stacking.clone());
        assert_eq!(
            bottle.replaced_addon_id(&second_dxvk),
            Some(first_dxvk.id())
        );
        bottle.put_addon(second_dxvk.clone());

        assert_eq!(bottle.addons().len(), 2);
        assert_eq!(bottle.addon(Slot::Dxvk).unwrap().id(), second_dxvk.id());
        assert!(
            bottle
                .addons()
                .iter()
                .any(|addon| addon.id() == stacking.id())
        );

        next_config::save(&path, &bottle).await.unwrap();
        let loaded: BottleState = next_config::load(&path).await.unwrap();
        assert_eq!(loaded.addon(Slot::Dxvk).unwrap().id(), second_dxvk.id());
        assert!(
            loaded
                .addons()
                .iter()
                .any(|addon| addon.id() == stacking.id())
        );
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}

#[test]
fn virgo_layers_round_trip_through_bottle_toml() {
    futures_lite::future::block_on(async {
        use fvs_rs::{Commit, Layer, Repository};

        let directories = test_directories();
        let id = uuid::Uuid::new_v4();
        let bottle_path = directories.bottle(id);
        std::fs::create_dir_all(&bottle_path).unwrap();
        let repository = Repository {
            repository_path: bottle_path.join("repo").display().to_string(),
            block_size: 4096,
        };
        let commit = Commit {
            repository_path: repository.repository_path.clone(),
            state_id: "state".into(),
            created_at: None,
            file_count: 1,
            message: "test".into(),
            created: true,
        };
        let expected = Layer::new(&repository, Some(&commit));
        let mut bottle = state(&directories, id, "virgo");
        bottle.storage = Prefix::Virgo {
            layers: vec![expected.clone()],
        };
        let path = bottle_path.join("bottle.toml");

        next_config::save(&path, &bottle).await.unwrap();
        let loaded: BottleState = next_config::load(&path).await.unwrap();
        assert_eq!(loaded.storage(), Storage::Virgo);
        assert_eq!(loaded.storage, bottle.storage);
        std::fs::remove_dir_all(directories.data_dir()).unwrap();
    });
}
