use crate::compatibility::{
    Architecture, Checksum,
    catalog::{Artifact, Catalog, CatalogItem},
    deserialize_non_empty_string,
    installer::InstallStep,
};
use serde::{Deserialize, Serialize};
use url::Url;
use uuid::{NonNilUuid, Uuid};

const CATALOG_VERSION: u32 = 1;

pub type DependencyCatalog = Catalog<CatalogDependencyEntry, CATALOG_VERSION>;

impl CatalogItem for CatalogDependencyEntry {
    fn uuid(&self) -> Uuid {
        self.uuid()
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogDependencyEntry {
    id: NonNilUuid,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    name: String,
    #[serde(deserialize_with = "deserialize_non_empty_string")]
    version: String,
    resources: Vec<DependencyResource>,
}

impl CatalogDependencyEntry {
    pub fn uuid(&self) -> Uuid {
        self.id.get()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn resources(&self) -> &[DependencyResource] {
        &self.resources
    }

    pub fn supports(&self, arch: Architecture) -> bool {
        self.resources
            .iter()
            .any(|resource| resource.supports(arch))
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyResource {
    #[serde(flatten)]
    artifact: Artifact,
    target_arch: Architecture,
    #[serde(default)]
    steps: Vec<InstallStep>,
}

impl DependencyResource {
    pub fn file_name(&self) -> &str {
        &self.artifact.file_name()
    }

    pub fn url(&self) -> &Url {
        &self.artifact.url()
    }

    pub fn checksum(&self) -> &Checksum {
        &self.artifact.checksum()
    }

    pub fn size(&self) -> Option<u64> {
        self.artifact.size()
    }

    pub fn target_arch(&self) -> Architecture {
        self.target_arch
    }

    pub fn steps(&self) -> &[InstallStep] {
        &self.steps
    }

    pub fn supports(&self, arch: Architecture) -> bool {
        self.target_arch == arch
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use uuid::uuid;

    use crate::{
        compatibility::installer::InstallStep,
        proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    };

    use super::*;

    const MANIFEST: &str = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "vcrun2022",
        "version": "14.38.33135",
        "resources": [
            {
                "file_name": "vc_redist.x86.exe",
                "url": "https://example.test/vc_redist.x86.exe",
                "checksum": { "algorithm": "sha256", "value": "abc" },
                "size": 123456,
                "target_arch": "x86",
                "steps": [
                    {
                        "action": "execute",
                        "arguments": ["/quiet", "/norestart"]
                    }
                ]
            },
            {
                "file_name": "vcruntime140.dll",
                "url": "https://example.test/vcruntime140.dll",
                "checksum": { "algorithm": "sha512", "value": "def" },
                "size": 654321,
                "target_arch": "x86_64",
                "steps": [
                    {
                        "action": "copy",
                        "destination": "drive_c/windows/system32/vcruntime140.dll"
                    },
                    {
                        "action": "register-dlls",
                        "dlls": ["drive_c/windows/system32/vcruntime140.dll"]
                    },
                    {
                        "action": "set-registry-value",
                        "hive": "current-user",
                        "key": "Software\\Example",
                        "name": "Installed",
                        "value": { "dword": 1 }
                    },
                    {
                        "action": "set-registry-value",
                        "hive": "local-machine",
                        "key": "Software\\Example",
                        "name": "Version",
                        "value": { "string": "14.38" }
                    },
                    {
                        "action": "set-dll-overrides",
                        "dlls": ["vcruntime140", "vcruntime140_1"],
                        "mode": "native-builtin"
                    }
                ]
            },
            {
                "file_name": "runtime.tar.gz",
                "url": "https://example.test/runtime.tar.gz",
                "checksum": { "algorithm": "sha256", "value": "ghi" },
                "size": 42,
                "target_arch": "x86_64",
                "steps": [
                    {
                        "action": "extract",
                        "destination": "drive_c/runtime"
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn deserializes_all_dependency_steps_in_order() {
        let dependency: CatalogDependencyEntry = serde_json::from_str(MANIFEST).unwrap();

        assert_eq!(
            dependency.uuid(),
            uuid!("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(dependency.name(), "vcrun2022");
        assert_eq!(dependency.version(), "14.38.33135");
        assert_eq!(dependency.resources().len(), 3);

        let x86 = &dependency.resources()[0];
        assert_eq!(x86.file_name(), "vc_redist.x86.exe");
        assert_eq!(x86.size(), Some(123456));
        assert_eq!(x86.target_arch(), Architecture::X86);
        assert!(matches!(
            &x86.steps()[0],
            InstallStep::Execute {
                arguments,
            } if arguments == &["/quiet", "/norestart"]
        ));

        let x64 = &dependency.resources()[1];
        assert_eq!(x64.checksum(), &Checksum::sha512("def"));
        assert!(matches!(
            &x64.steps()[0],
            InstallStep::Copy { source, destination }
                if source.as_os_str().is_empty()
                    && destination == Path::new("drive_c/windows/system32/vcruntime140.dll")
        ));
        assert!(matches!(
            &x64.steps()[1],
            InstallStep::RegisterDlls { dlls }
                if dlls == &[Path::new("drive_c/windows/system32/vcruntime140.dll")]
        ));
        assert!(matches!(
            &dependency.resources()[2].steps()[0],
            InstallStep::Extract { destination }
                if destination == Path::new("drive_c/runtime")
        ));

        assert!(matches!(
            &x64.steps()[3],
            InstallStep::SetRegistryValue {
                value: RegistryValue::String(value), ..
            } if value == "14.38"
        ));
        assert!(matches!(
            &x64.steps()[4],
            InstallStep::SetDllOverrides {
                dlls,
                mode: DllOverrideMode::NativeBuiltin,
            } if dlls == &["vcruntime140", "vcruntime140_1"]
        ));
    }

    #[test]
    fn preserves_resource_and_step_order_when_round_tripped() {
        let dependency: CatalogDependencyEntry = serde_json::from_str(MANIFEST).unwrap();
        let serialized = serde_json::to_string(&dependency).unwrap();
        let round_tripped: CatalogDependencyEntry = serde_json::from_str(&serialized).unwrap();

        assert_eq!(round_tripped, dependency);
    }

    #[test]
    fn execute_options_default_to_empty() {
        let mut value = serde_json::from_str::<serde_json::Value>(MANIFEST).unwrap();
        value["resources"][0]["steps"][0] = serde_json::json!({ "action": "execute" });
        let dependency: CatalogDependencyEntry = serde_json::from_value(value).unwrap();

        assert!(matches!(
            &dependency.resources()[0].steps()[0],
            InstallStep::Execute {
                arguments,
            } if arguments.is_empty()
        ));
    }

    #[test]
    fn deserializes_environment_steps() {
        let step: InstallStep = serde_json::from_value(serde_json::json!({
            "action": "set-environment",
            "name": "EXAMPLE",
            "value": "enabled"
        }))
        .unwrap();

        assert!(matches!(
            step,
            InstallStep::SetEnvironment { name, value }
                if name == "EXAMPLE" && value == "enabled"
        ));
    }

    #[test]
    fn matches_resources_by_exact_target_architecture() {
        let dependency: CatalogDependencyEntry = serde_json::from_str(MANIFEST).unwrap();
        let x86 = &dependency.resources()[0];
        let x64 = &dependency.resources()[1];

        assert!(x86.supports(Architecture::X86));
        assert!(!x86.supports(Architecture::X86_64));
        assert!(!x86.supports(Architecture::Aarch64));
        assert!(!x64.supports(Architecture::X86));
        assert!(x64.supports(Architecture::X86_64));
        assert!(!x64.supports(Architecture::Aarch64));
    }

    #[test]
    fn deserializes_every_registry_value_type() {
        for value in [
            serde_json::json!({ "none": [1, 2] }),
            serde_json::json!({ "binary": [3, 4] }),
            serde_json::json!({ "dword": 5 }),
            serde_json::json!({ "qword": 6 }),
            serde_json::json!({ "expand-string": "%PATH%" }),
            serde_json::json!({ "multi-string": { "values": ["a", "b"] } }),
            serde_json::json!({ "string": "text" }),
        ] {
            let parsed: RegistryValue = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        }
    }

    #[test]
    fn deserializes_every_registry_hive_and_dll_override_mode() {
        for hive in [
            "classes-root",
            "current-user",
            "local-machine",
            "users",
            "current-config",
        ] {
            let parsed: RegistryHive = serde_json::from_str(&format!(r#""{hive}""#)).unwrap();
            let _ = parsed;
        }

        for mode in [
            "native-builtin",
            "builtin-native",
            "native",
            "builtin",
            "disabled",
        ] {
            let parsed: DllOverrideMode = serde_json::from_str(&format!(r#""{mode}""#)).unwrap();
            let _ = parsed;
        }
    }

    #[test]
    fn rejects_invalid_and_unknown_manifest_fields() {
        let invalid = [
            MANIFEST.replace(r#""name": "vcrun2022""#, r#""name": """#),
            MANIFEST.replace(r#""size": 123456"#, r#""size": 0"#),
            MANIFEST.replace(r#""value": "abc""#, r#""value": """#),
            MANIFEST.replace(r#""key": "Software\\Example""#, r#""key": " ""#),
        ];

        for manifest in invalid {
            assert!(serde_json::from_str::<CatalogDependencyEntry>(&manifest).is_err());
        }

        let mut unknown_dependency = serde_json::from_str::<serde_json::Value>(MANIFEST).unwrap();
        unknown_dependency["unexpected"] = true.into();
        assert!(serde_json::from_value::<CatalogDependencyEntry>(unknown_dependency).is_err());

        let mut unknown_step = serde_json::from_str::<serde_json::Value>(MANIFEST).unwrap();
        unknown_step["resources"][0]["steps"][0]["unexpected"] = true.into();
        assert!(serde_json::from_value::<CatalogDependencyEntry>(unknown_step).is_err());

        let mut unknown_resource_action =
            serde_json::from_str::<serde_json::Value>(MANIFEST).unwrap();
        unknown_resource_action["resources"][0]["steps"][0]["action"] = "unknown".into();
        assert!(serde_json::from_value::<CatalogDependencyEntry>(unknown_resource_action).is_err());
    }
}
