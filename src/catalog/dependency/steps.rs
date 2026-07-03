use crate::{catalog::deserialize_non_empty_string, runner::WindowsVersion};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf};

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DependencyStep {
    Copy {
        destination: PathBuf,
    },
    Execute {
        #[serde(default)]
        arguments: Vec<String>,
        #[serde(default)]
        environment: HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        windows_version: Option<WindowsVersion>,
    },
    Extract {
        destination: PathBuf,
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

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryHive {
    ClassesRoot,
    CurrentUser,
    LocalMachine,
    Users,
    CurrentConfig,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum RegistryValue {
    None(Vec<u8>),
    Binary(Vec<u8>),
    Dword(u32),
    Qword(u64),
    ExpandString(String),
    MultiString(Vec<String>),
    String(String),
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DllOverrideMode {
    NativeBuiltin,
    BuiltinNative,
    Native,
    Builtin,
    Disabled,
}
