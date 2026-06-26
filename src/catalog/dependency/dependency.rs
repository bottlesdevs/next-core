use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use url::Url;
use uuid::{NonNilUuid, Uuid};

use crate::catalog::{Architecture, Checksum};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct Dependency {
    id: NonNilUuid,
    name: String,
    version: String,
    #[serde(flatten)]
    source: DependencySource,
}

impl Dependency {
    pub fn uuid(&self) -> Uuid {
        self.id.get()
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn source(&self) -> &DependencySource {
        &self.source
    }
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum DependencySource {
    Files { files: Vec<DependencyFile> },
    Artifacts { artifacts: Vec<DependencyArtifact> },
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyFile {
    file_name: String,
    destination: PathBuf,
    url: Url,
    checksum: Checksum,
    size: usize,
    arch: Architecture,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyArtifact {
    file_name: String,
    url: Url,
    checksum: Checksum,
    size: usize,
    arch: Architecture,
}

#[cfg(test)]
mod tests {
    use uuid::uuid;

    use super::*;

    #[test]
    fn deserializes_dependency_with_files() {
        let dependency: Dependency = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "name": "vcrun2022",
                "version": "14.38.33135",
                "files": [
                    {
                        "file_name": "vcruntime140.dll",
                        "destination": "drive_c/windows/system32/vcruntime140.dll",
                        "url": "https://example.test/vcruntime140.dll",
                        "checksum": {
                            "algorithm": "sha256",
                            "value": "abc"
                        },
                        "size": 123456,
                        "arch": "x86_64"
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            dependency.uuid(),
            uuid!("00000000-0000-0000-0000-000000000001")
        );
        assert_eq!(dependency.name(), "vcrun2022");
        assert_eq!(dependency.version(), "14.38.33135");

        let DependencySource::Files { files } = dependency.source() else {
            panic!("expected files dependency source");
        };

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].file_name, "vcruntime140.dll");
        assert_eq!(
            files[0].destination,
            PathBuf::from("drive_c/windows/system32/vcruntime140.dll")
        );
        assert_eq!(
            files[0].url,
            Url::parse("https://example.test/vcruntime140.dll").unwrap()
        );
        assert_eq!(files[0].checksum, Checksum::sha256("abc"));
        assert_eq!(files[0].size, 123456);
        assert_eq!(files[0].arch, Architecture::X86_64);
    }

    #[test]
    fn deserializes_dependency_with_artifacts() {
        let dependency: Dependency = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000002",
                "name": "dxvk-runtime",
                "version": "2.4",
                "artifacts": [
                    {
                        "file_name": "dxvk-runtime.tar.gz",
                        "url": "https://example.test/dxvk-runtime.tar.gz",
                        "checksum": {
                            "algorithm": "sha512",
                            "value": "def"
                        },
                        "size": 654321,
                        "arch": "x86_64"
                    }
                ]
            }"#,
        )
        .unwrap();

        assert_eq!(
            dependency.uuid(),
            uuid!("00000000-0000-0000-0000-000000000002")
        );
        assert_eq!(dependency.name(), "dxvk-runtime");
        assert_eq!(dependency.version(), "2.4");

        let DependencySource::Artifacts { artifacts } = dependency.source() else {
            panic!("expected artifacts dependency source");
        };

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].file_name, "dxvk-runtime.tar.gz");
        assert_eq!(
            artifacts[0].url,
            Url::parse("https://example.test/dxvk-runtime.tar.gz").unwrap()
        );
        assert_eq!(artifacts[0].checksum, Checksum::sha512("def"));
        assert_eq!(artifacts[0].size, 654321);
        assert_eq!(artifacts[0].arch, Architecture::X86_64);
    }

    #[test]
    fn rejects_dependency_without_source() {
        let result = serde_json::from_str::<Dependency>(
            r#"{
                "id": "00000000-0000-0000-0000-000000000003",
                "name": "missing-source",
                "version": "1"
            }"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn rejects_dependency_with_files_and_artifacts() {
        let result = serde_json::from_str::<Dependency>(
            r#"{
                "id": "00000000-0000-0000-0000-000000000004",
                "name": "ambiguous-source",
                "version": "1",
                "files": [
                    {
                        "file_name": "vcruntime140.dll",
                        "destination": "drive_c/windows/system32/vcruntime140.dll",
                        "url": "https://example.test/vcruntime140.dll",
                        "checksum": {
                            "algorithm": "sha256",
                            "value": "abc"
                        },
                        "size": 123456,
                        "arch": "x86_64"
                    }
                ],
                "artifacts": [
                    {
                        "file_name": "vc_redist.x64.exe",
                        "url": "https://example.test/vc_redist.x64.exe",
                        "checksum": {
                            "algorithm": "sha256",
                            "value": "def"
                        },
                        "size": 654321,
                        "arch": "x86_64"
                    }
                ]
            }"#,
        );

        assert!(result.is_err());
    }
}
