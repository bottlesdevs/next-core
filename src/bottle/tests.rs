#[cfg(unix)]
mod unix {
    use std::{fs, os::unix::fs::PermissionsExt};

    use uuid::Uuid;

    use super::super::*;

    fn install_wine(data_root: &std::path::Path, key: &str) {
        let bin = data_root.join("runners").join(key).join("bin");
        fs::create_dir_all(&bin).unwrap();
        for (name, script) in [
            (
                "wine",
                "#!/bin/sh\nmkdir -p \"$WINEPREFIX\"\ntouch \"$WINEPREFIX/initialized\"\n",
            ),
            ("wineserver", "#!/bin/sh\nexit 0\n"),
        ] {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[tokio::test]
    async fn manually_installed_runner_and_programs_are_stored_in_toml() {
        let directories = crate::directories::expect();
        let runner_key = format!("manual-wine-{}", Uuid::new_v4());
        let manager = BottleManager::new(BottleManagerConfig {
            winebridge_executable: directories.data_dir().join("bridge.exe"),
            fvs2d_executable: None,
            umu_executable: None,
        })
        .unwrap();
        install_wine(directories.data_dir(), &runner_key);

        let mut bottle = manager
            .create(
                Uuid::new_v4().to_string(),
                BottleType::Standard,
                runner_key.clone(),
            )
            .await
            .unwrap();
        let program = Program::new("Game", "C:\\game.exe");
        let program_id = program.id;
        bottle.add_program(program).unwrap();
        let bottle_id = bottle.id();
        drop(bottle);

        let reopened = manager.open(bottle_id).unwrap();
        assert_eq!(reopened.runner(), runner_key);
        assert_eq!(reopened.r#type(), BottleType::Standard);
        assert_eq!(reopened.program(program_id).unwrap().name, "Game");
        let stored = fs::read_to_string(directories.bottle(bottle_id).join("bottle.toml")).unwrap();
        assert!(stored.contains(&format!("runner = \"{runner_key}\"")));
        assert!(stored.contains("[storage]"));
        assert!(stored.contains("kind = \"standard\""));
        assert!(!stored.contains("[prefix]"));
        assert!(!stored.contains("prefix_arch"));
        assert!(!stored.contains("environment"));
        assert!(!stored.contains(&directories.bottle(bottle_id).display().to_string()));
        assert!(stored.contains("[[programs]]"));
        assert!(
            directories
                .bottle(bottle_id)
                .join("prefix/initialized")
                .is_file()
        );

        drop(reopened);
        fs::remove_dir_all(directories.bottle(bottle_id)).unwrap();
        fs::remove_dir_all(directories.runner(&runner_key)).unwrap();
    }
}

#[test]
fn virgo_layers_round_trip_through_bottle_toml() {
    use fvs_rs::{Commit, Layer, Repository};

    let id = uuid::Uuid::new_v4();
    let root = crate::directories::expect().bottle(id);
    let repository = Repository {
        repository_path: root.join("repo").display().to_string(),
        block_size: 4096,
    };
    let commit = Commit {
        repository_path: repository.repository_path.clone(),
        state_id: "state".into(),
        created_at: None,
        file_count: 1,
        message: "test".into(),
    };
    let expected = Layer::new(&repository, Some(&commit));
    let bottle = super::bottle::Bottle {
        id,
        name: "virgo".into(),
        runner: "wine".into(),
        storage: super::bottle::PrefixStorage::Virgo {
            layers: vec![expected.clone()],
        },
        programs: Vec::new(),
        bridge: None,
    };
    let path = root.join("bottle.toml");

    next_config::save(&path, &bottle).unwrap();
    let loaded: super::bottle::Bottle = next_config::load(&path).unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("[storage]"));
    assert!(stored.contains("kind = \"virgo\""));
    assert!(!stored.contains("[prefix]"));
    assert!(!stored.contains(&format!("path = \"{}\"", root.join("prefix").display())));
    assert_eq!(loaded.r#type(), super::bottle::BottleType::Virgo);
    let super::bottle::PrefixStorage::Virgo { layers } = loaded.storage else {
        panic!("expected Virgo storage");
    };
    assert_eq!(layers, vec![expected]);

    std::fs::remove_dir_all(root).unwrap();
}
