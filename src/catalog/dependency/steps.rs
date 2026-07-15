use crate::{
    catalog::deserialize_non_empty_string,
    proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue},
    runner::WindowsVersion,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DependencyStep {
    Copy {
        destination: PathBuf,
    },
    Execute {
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        windows_version: Option<WindowsVersion>,
    },
    Extract {
        destination: PathBuf,
    },
    RegisterDlls {
        dlls: Vec<PathBuf>,
    },
    SetRegistryValue {
        hive: RegistryHive,
        #[serde(deserialize_with = "deserialize_non_empty_string")]
        key: String,
        name: String,
        value: RegistryValue,
    },
    SetDllOverrides {
        dlls: Vec<String>,
        mode: DllOverrideMode,
    },
}
