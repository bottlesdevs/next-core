use crate::{
    bottle::{
        bottle::{BottleComponents, BottleConfig},
        error::BottleError,
    },
    compatibility::{
        components::{
            Component,
            catalog::{ComponentKind, RunnerKind},
        },
        dependencies::Dependency,
    },
    wrapper::gamescope::{GamescopeConfig, Scaler},
};

#[test]
fn proton_umu_components_and_dependencies_round_trip() {
    let id = uuid::Uuid::new_v4();
    let bottle_path = crate::utils::directories::expect().bottle(id);
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
    let config = BottleConfig {
        id,
        name: "proton".into(),
        components,
        dependencies: vec![dependency],
        storage: super::bottle::PrefixStorage::Standard,
        programs: Vec::new(),
        gamescope: GamescopeConfig {
            game_width: Some(1280),
            scaler: Some(Scaler::Fit),
            fullscreen: true,
            ..Default::default()
        },
        environment: [("EXAMPLE".into(), "enabled".into())].into(),
    };
    let path = bottle_path.join("bottle.toml");

    next_config::save(&path, &config).unwrap();
    let loaded: BottleConfig = next_config::load(&path).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("[umu]"));
    assert!(stored.contains("[dxvk]"));
    assert!(stored.contains("[gamescope]"));
    assert!(stored.contains("[[dependencies]]"));
    assert_eq!(
        loaded.components.runner().kind(),
        ComponentKind::Runner {
            kind: RunnerKind::Proton
        }
    );
    assert_eq!(loaded.components.umu().unwrap().version(), "umu-1");
    assert_eq!(loaded.dependencies[0].name(), "vcrun2022");
    assert_eq!(loaded.gamescope, config.gamescope);
    assert_eq!(loaded.environment["EXAMPLE"], "enabled");

    std::fs::remove_dir_all(bottle_path).unwrap();
}

#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use uuid::Uuid;

    use super::super::*;
    use super::*;

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
        let directories = crate::utils::directories::expect();
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
        let manager = BottleManager::new(runner_root.join("bin/wine")).unwrap();
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
        )
        .await
        .unwrap();
        let mut bottle = Bottle::new(
            id,
            Uuid::new_v4().to_string(),
            BottleComponents::new(&runner, &bridge, None).unwrap(),
            Vec::new(),
            storage,
        )
        .unwrap();
        let program = Program::new("Game", "C:\\game.exe");
        let program_id = program.id;
        bottle.add_program(program).unwrap();
        let bottle_id = bottle.id();
        let failed_program = Program::new("Unsaved", "C:\\unsaved.exe");
        let failed_program_id = failed_program.id;
        let temporary = directories.bottle(bottle_id).join("bottle.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(bottle.add_program(failed_program).is_err());
        assert!(bottle.program(failed_program_id).is_none());
        let persisted: BottleConfig =
            next_config::load(directories.bottle(bottle_id).join("bottle.toml")).unwrap();
        assert!(
            persisted
                .programs
                .iter()
                .all(|program| program.id != failed_program_id)
        );
        fs::remove_dir(temporary).unwrap();
        let runner_id = bottle.runner().id();
        drop(bottle);

        let mut reopened = manager.open(bottle_id).unwrap();
        assert_eq!(reopened.runner().id(), runner_id);
        assert_eq!(reopened.runner().path(), runner_root);
        assert_eq!(reopened.r#type(), BottleType::Standard);
        assert_eq!(reopened.program(program_id).unwrap().name, "Game");
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

        drop(reopened);
        fs::remove_dir_all(directories.bottle(bottle_id)).unwrap();
        fs::remove_dir_all(assets).unwrap();
    }
}

#[test]
fn virgo_layers_round_trip_through_bottle_toml() {
    use fvs_rs::{Commit, Layer, Repository};

    let id = uuid::Uuid::new_v4();
    let bottle_path = crate::utils::directories::expect().bottle(id);
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
    let config = BottleConfig {
        id,
        name: "virgo".into(),
        components: BottleComponents::new(&runner, &bridge, None).unwrap(),
        dependencies: Vec::new(),
        storage: super::bottle::PrefixStorage::Virgo {
            layers: vec![expected.clone()],
        },
        programs: Vec::new(),
        gamescope: GamescopeConfig::default(),
        environment: Default::default(),
    };
    let path = bottle_path.join("bottle.toml");

    next_config::save(&path, &config).unwrap();
    let loaded: BottleConfig = next_config::load(&path).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("[runner]"));
    assert!(stored.contains("[winebridge]"));
    assert!(stored.contains("[storage]"));
    assert!(!stored.contains("[prefix]"));
    assert!(!stored.contains(&format!(
        "path = \"{}\"",
        bottle_path.join("prefix").display()
    )));
    assert_eq!(loaded.storage.kind(), super::bottle::BottleType::Virgo);
    let super::bottle::PrefixStorage::Virgo { layers } = loaded.storage else {
        panic!("expected Virgo storage");
    };
    assert_eq!(layers, vec![expected]);

    std::fs::remove_dir_all(bottle_path).unwrap();
}
