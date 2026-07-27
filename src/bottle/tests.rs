use crate::{
    Context, Directories,
    bottle::{
        BottleManager,
        bottle::{BottleComponents, BottleState},
        error::BottleError,
    },
    compatibility::{
        components::{Component, catalog::ComponentKind},
        dependencies::Dependency,
    },
    runner::RunnerKind,
    utils::environment::Environment,
    wrapper::{
        Wrappers,
        gamescope::{GamescopeConfig, Scaler},
        mangohud::MangoHudConfig,
    },
};

fn test_directories() -> Directories {
    let root = std::env::temp_dir().join(format!("bottles-next-{}", uuid::Uuid::new_v4()));
    Directories::from_path(root).unwrap()
}

#[tokio::test]
async fn bottle_managers_are_scoped_to_their_context_roots() {
    let id = uuid::Uuid::new_v4();
    let left = test_directories();
    let right = test_directories();

    for (directories, name) in [(&left, "left"), (&right, "right")] {
        std::fs::create_dir_all(directories.bottle(id)).unwrap();
        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "wine",
            directories.data_dir().join("runner"),
        )
        .unwrap();
        let bridge = Component::new(
            ComponentKind::Winebridge,
            "bridge",
            directories.data_dir().join("winebridge.exe"),
        )
        .unwrap();
        let config = BottleState {
            id,
            name: name.into(),
            storage: super::PrefixStorage::Standard,
            programs: Vec::new(),
            components: BottleComponents::new(&runner, &bridge, None).unwrap(),
            dependencies: Vec::new(),
            environment: Default::default(),
            wrappers: Wrappers::default(),
        };
        next_config::save(directories.bottle(id).join("bottle.toml"), &config).unwrap();
    }

    let left_manager =
        BottleManager::new(Context::new(left.clone(), left.data_dir().join("fvs2d")).unwrap());
    let right_manager =
        BottleManager::new(Context::new(right.clone(), right.data_dir().join("fvs2d")).unwrap());

    assert_eq!(left_manager.open(id).await.unwrap().state().name(), "left");
    assert_eq!(
        right_manager.open(id).await.unwrap().state().name(),
        "right"
    );
    assert_eq!(
        left_manager.list().await.unwrap()[0]
            .as_ref()
            .unwrap()
            .state()
            .name(),
        "left"
    );
    assert_eq!(
        right_manager.list().await.unwrap()[0]
            .as_ref()
            .unwrap()
            .state()
            .name(),
        "right"
    );

    std::fs::remove_dir_all(left.data_dir()).unwrap();
    std::fs::remove_dir_all(right.data_dir()).unwrap();
}

#[tokio::test]
async fn list_reports_corrupt_bottles() {
    let directories = test_directories();
    let id = uuid::Uuid::new_v4();
    std::fs::create_dir_all(directories.bottle(id)).unwrap();
    std::fs::write(
        directories.bottle(id).join("bottle.toml"),
        "not valid toml =",
    )
    .unwrap();
    let manager = BottleManager::new(
        Context::new(directories.clone(), directories.data_dir().join("fvs2d")).unwrap(),
    );

    let bottles = manager.list().await.unwrap();

    assert_eq!(bottles.len(), 1);
    assert!(bottles[0].is_err());
    std::fs::remove_dir_all(directories.data_dir()).unwrap();
}

#[test]
fn proton_umu_components_and_dependencies_round_trip() {
    let directories = test_directories();
    let id = uuid::Uuid::new_v4();
    let bottle_path = directories.bottle(id);
    std::fs::create_dir_all(&bottle_path).unwrap();
    let proton = Component::new(
        ComponentKind::Runner {
            kind: RunnerKind::Proton,
        },
        "proton-1",
        bottle_path.join("proton"),
    )
    .unwrap();
    let wine = Component::new(
        ComponentKind::Runner {
            kind: RunnerKind::Wine,
        },
        "wine-1",
        bottle_path.join("wine"),
    )
    .unwrap();
    let bridge = Component::new(
        ComponentKind::Winebridge,
        "bridge-1",
        bottle_path.join("winebridge/bottles-winebridge.exe"),
    )
    .unwrap();
    let umu = Component::new(ComponentKind::Umu, "umu-1", bottle_path.join("umu/umu-run")).unwrap();
    let dxvk = Component::new(ComponentKind::Dxvk, "dxvk-1", bottle_path.join("dxvk")).unwrap();
    assert!(matches!(
        BottleComponents::new(&proton, &bridge, None),
        Err(crate::error::Error::Bottle(
            BottleError::ProtonRunnerWithoutUmu
        ))
    ));
    assert!(matches!(
        BottleComponents::new(&wine, &bridge, Some(&umu)),
        Err(crate::error::Error::Bottle(BottleError::WineRunnerWithUmu))
    ));
    let mut components = BottleComponents::new(&proton, &bridge, Some(&umu)).unwrap();
    components.dxvk = Some(dxvk);
    let dependency: Dependency = serde_json::from_value(serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "vcrun2022",
        "version": "14.38"
    }))
    .unwrap();
    let mut environment = Environment::default();
    environment.insert("EXAMPLE".into(), "enabled".into());
    let config = BottleState {
        id,
        name: "proton".into(),
        components,
        dependencies: vec![dependency],
        storage: super::PrefixStorage::Standard,
        programs: Vec::new(),
        wrappers: Wrappers {
            gamescope: GamescopeConfig {
                enabled: true,
                game_width: Some(1280),
                scaler: Some(Scaler::Fit),
                fullscreen: true,
                ..Default::default()
            },
            mangohud: MangoHudConfig { enabled: true },
        },
        environment,
    };
    let path = bottle_path.join("bottle.toml");

    next_config::save(&path, &config).unwrap();
    let loaded: BottleState = next_config::load(&path).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("[umu]"));
    assert!(stored.contains("[dxvk]"));
    assert!(stored.contains("[gamescope]"));
    assert!(stored.contains("[mangohud]"));
    assert!(stored.contains("enabled = true"));
    assert!(stored.contains("[[dependencies]]"));
    assert_eq!(
        loaded.components.runner().kind(),
        ComponentKind::Runner {
            kind: RunnerKind::Proton
        }
    );
    assert_eq!(loaded.components.umu().unwrap().version(), "umu-1");
    assert_eq!(loaded.dependencies[0].name(), "vcrun2022");
    assert_eq!(loaded.wrappers, config.wrappers);
    assert_eq!(loaded.environment, config.environment);

    std::fs::remove_dir_all(directories.data_dir()).unwrap();
}

#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use uuid::Uuid;

    use super::super::*;
    use super::*;
    use crate::bottle::PrefixStorage;

    fn install_wine(runner_path: &Path) {
        let bin = runner_path.join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (name, script) in [
            (
                "wine",
                "#!/bin/sh\nmkdir -p \"$WINEPREFIX\"\ntouch \"$WINEPREFIX/initialized\"\n",
            ),
            (
                "wineserver",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$WINEPREFIX/wineserver.log\"\n",
            ),
        ] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[tokio::test]
    async fn components_and_programs_round_trip_through_bottle_toml() {
        let directories = test_directories();
        let context =
            Context::new(directories.clone(), directories.data_dir().join("fvs2d")).unwrap();
        let assets = directories
            .data_dir()
            .join(format!("test-assets-{}", Uuid::new_v4()));
        let runner_root = assets.join("wine");
        let bridge_root = assets.join("winebridge");
        install_wine(&runner_root);
        fs::create_dir_all(&bridge_root).unwrap();
        fs::write(bridge_root.join("bottles-winebridge.exe"), []).unwrap();

        let runner = Component::new(
            ComponentKind::Runner {
                kind: RunnerKind::Wine,
            },
            "manual-wine",
            &runner_root,
        )
        .unwrap();
        let bridge = Component::new(
            ComponentKind::Winebridge,
            "manual-winebridge",
            bridge_root.join("bottles-winebridge.exe"),
        )
        .unwrap();
        let manager = BottleManager::new(context.clone());
        let id = Uuid::new_v4();
        let bottle_path = directories.bottle(id);
        fs::create_dir_all(&bottle_path).unwrap();
        let storage = PrefixStorage::create(
            BottleType::Standard,
            &bottle_path,
            crate::runner::load_runner(&runner_root, RunnerKind::Wine, None)
                .unwrap()
                .as_ref(),
            &runner.id().to_string(),
            &context,
        )
        .await
        .unwrap();
        let bottle = Bottle::new(
            id,
            Uuid::new_v4().to_string(),
            BottleComponents::new(&runner, &bridge, None).unwrap(),
            Vec::new(),
            storage,
            context,
        )
        .await
        .unwrap();
        let program = Program::new("Game", "C:\\game.exe");
        let program_id = program.id;
        let wrappers = Wrappers {
            gamescope: GamescopeConfig {
                enabled: true,
                fullscreen: true,
                ..Default::default()
            },
            mangohud: MangoHudConfig { enabled: true },
        };
        let mut edit = bottle.edit();
        assert_eq!(bottle.state().id(), id);
        edit.add_program(program)
            .set_gamescope(GamescopeConfig {
                enabled: true,
                fullscreen: true,
                ..Default::default()
            })
            .set_mangohud(MangoHudConfig { enabled: true });
        edit.commit().await.unwrap();
        assert_eq!(bottle.state().wrappers(), &wrappers);
        let bottle_id = bottle.state().id();
        let failed_program = Program::new("Unsaved", "C:\\unsaved.exe");
        let failed_program_id = failed_program.id;
        let temporary = directories.bottle(bottle_id).join("bottle.tmp");
        fs::create_dir(&temporary).unwrap();
        let mut edit = bottle.edit();
        edit.add_program(failed_program);
        assert!(edit.commit().await.is_err());
        let bottle = manager.open(bottle_id).await.unwrap();
        assert!(bottle.state().program(failed_program_id).is_none());
        let persisted: BottleState =
            next_config::load(directories.bottle(bottle_id).join("bottle.toml")).unwrap();
        assert!(
            persisted
                .programs
                .iter()
                .all(|program| program.id != failed_program_id)
        );
        fs::remove_dir(temporary).unwrap();
        let runner_id = bottle.state().runner().id();
        drop(bottle);

        let reopened = manager.open(bottle_id).await.unwrap();
        assert_eq!(reopened.state().runner().id(), runner_id);
        assert_eq!(reopened.state().runner().path(), runner_root);
        assert_eq!(reopened.state().r#type(), BottleType::Standard);
        assert_eq!(reopened.state().program(program_id).unwrap().name, "Game");
        assert_eq!(reopened.state().wrappers(), &wrappers);
        let same = manager.open(bottle_id).await.unwrap();
        let mut edit = reopened.edit();
        edit.rename("Shared");
        edit.commit().await.unwrap();
        assert_eq!(same.state().name(), "Shared");
        let stored = fs::read_to_string(directories.bottle(bottle_id).join("bottle.toml")).unwrap();
        assert!(stored.contains("[runner]"));
        assert!(stored.contains("type = \"runner\""));
        assert!(stored.contains("runner = \"wine\""));
        assert!(stored.contains("[winebridge]"));
        assert!(!stored.contains("[[runner]]"));
        assert!(!stored.contains("[umu]"));
        assert!(stored.contains("[storage]"));
        assert!(!stored.contains("[prefix]"));
        assert!(!stored.contains("environment"));
        assert!(stored.contains("[[programs]]"));
        assert!(
            directories
                .bottle(bottle_id)
                .join("prefix/initialized")
                .is_file()
        );
        assert_eq!(
            fs::read_to_string(directories.bottle(bottle_id).join("prefix/wineserver.log"))
                .unwrap(),
            "-k\n"
        );

        reopened.stop().await.unwrap();
        assert_eq!(
            fs::read_to_string(directories.bottle(bottle_id).join("prefix/wineserver.log"))
                .unwrap(),
            "-k\n-k\n"
        );

        manager.delete(bottle_id).await.unwrap();
        let edit = same.edit();
        assert!(matches!(
            edit.commit().await,
            Err(crate::error::Error::Bottle(BottleError::Deleted(id))) if id == bottle_id
        ));
        assert!(matches!(
            manager.open(bottle_id).await,
            Err(crate::error::Error::Bottle(BottleError::NotFound(id))) if id == bottle_id
        ));
        fs::remove_dir_all(assets).unwrap();
        fs::remove_dir_all(directories.data_dir()).unwrap();
    }
}

#[test]
fn virgo_layers_round_trip_through_bottle_toml() {
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
    let runner = Component::new(
        ComponentKind::Runner {
            kind: RunnerKind::Wine,
        },
        "wine",
        bottle_path.join("runner"),
    )
    .unwrap();
    let bridge = Component::new(
        ComponentKind::Winebridge,
        "winebridge",
        bottle_path.join("winebridge/bottles-winebridge.exe"),
    )
    .unwrap();
    let config = BottleState {
        id,
        name: "virgo".into(),
        components: BottleComponents::new(&runner, &bridge, None).unwrap(),
        dependencies: Vec::new(),
        storage: super::PrefixStorage::Virgo {
            layers: vec![expected.clone()],
        },
        programs: Vec::new(),
        wrappers: Wrappers::default(),
        environment: Default::default(),
    };
    let path = bottle_path.join("bottle.toml");

    next_config::save(&path, &config).unwrap();
    let loaded: BottleState = next_config::load(&path).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("[runner]"));
    assert!(stored.contains("[winebridge]"));
    assert!(stored.contains("[storage]"));
    assert!(!stored.contains("[prefix]"));
    assert!(!stored.contains(&format!(
        "path = \"{}\"",
        bottle_path.join("prefix").display()
    )));
    assert_eq!(loaded.kind(), super::bottle::BottleType::Virgo);
    assert!(stored.contains("state"));

    std::fs::remove_dir_all(directories.data_dir()).unwrap();
}
