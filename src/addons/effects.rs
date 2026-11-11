//! Effects a release applies each time Wine starts.
//!
//! Effects are derived from the frozen release and the prefix's setting values
//! on every startup, so changing a setting never rebuilds or reinstalls
//! anything. They are listed with the settings in an
//! [`AddonSettingsSchema`](super::AddonSettingsSchema), which checks each
//! condition and template against those settings.

use serde::{Deserialize, Serialize};

use super::SettingValue;
use crate::proto::{DllOverrideMode, RegistryHive, registry_value::Value as RegistryValue};

/// Describes one thing a release sets when Wine starts, applied only while
/// its condition holds.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "action", deny_unknown_fields)]
pub(crate) enum Effect {
    /// Sets an environment variable of the started `WineBridge`.
    #[serde(rename = "set-environment")]
    Environment {
        /// Variable name.
        name: String,
        /// Value, which may contain `{{ key }}` placeholders.
        value: String,
        /// Condition under which the effect applies; always when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Condition>,
    },
    /// Applies one DLL override mode to each named DLL.
    #[serde(rename = "set-dll-overrides")]
    DllOverrides {
        /// DLL names, without extension.
        dlls: Vec<String>,
        /// Mode applied to every DLL.
        mode: DllOverrideMode,
        /// Condition under which the effect applies; always when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Condition>,
    },
    /// Writes a registry value once `WineBridge` is reachable.
    #[serde(rename = "set-registry-value")]
    RegistryValue {
        /// Root hive holding the key.
        hive: RegistryHive,
        /// Key path below the hive.
        key: String,
        /// Value name; empty for the key's default value.
        name: String,
        /// Data written. String data may contain `{{ key }}` placeholders.
        value: RegistryValue,
        /// Condition under which the effect applies; always when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when: Option<Condition>,
    },
}

impl Effect {
    /// Returns the condition under which this effect applies.
    pub(crate) fn when(&self) -> Option<&Condition> {
        match self {
            Self::Environment { when, .. }
            | Self::DllOverrides { when, .. }
            | Self::RegistryValue { when, .. } => when.as_ref(),
        }
    }

    /// Returns the text that may contain `{{ key }}` placeholders.
    pub(crate) fn template(&self) -> Option<&str> {
        match self {
            Self::Environment { value, .. }
            | Self::RegistryValue {
                value: RegistryValue::String(value) | RegistryValue::ExpandString(value),
                ..
            } => Some(value),
            _ => None,
        }
    }

    /// Returns this effect with each text that may contain placeholders
    /// replaced by `render` of it.
    pub(crate) fn rendered(&self, render: impl Fn(&str) -> String) -> Self {
        let mut effect = self.clone();
        if let Self::Environment { value, .. }
        | Self::RegistryValue {
            value: RegistryValue::String(value) | RegistryValue::ExpandString(value),
            ..
        } = &mut effect
        {
            *value = render(value);
        }
        effect
    }
}

/// Compares the value in effect of one setting with a fixed value.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Condition {
    /// Key of the compared setting.
    pub(crate) setting: String,
    #[serde(flatten)]
    test: Test,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Test {
    Equals(SettingValue),
    NotEquals(SettingValue),
}

impl Condition {
    /// Returns the value the setting is compared with.
    pub(crate) fn value(&self) -> &SettingValue {
        match &self.test {
            Test::Equals(value) | Test::NotEquals(value) => value,
        }
    }

    /// Returns whether the condition holds when the setting has `actual`.
    pub(crate) fn holds(&self, actual: &SettingValue) -> bool {
        match &self.test {
            Test::Equals(value) => actual == value,
            Test::NotEquals(value) => actual != value,
        }
    }
}

/// Replaces each `{{ key }}` in `text` with `value(key)`.
///
/// Fails with the key `value` does not know, or with `None` for an unclosed
/// placeholder.
pub(crate) fn render(
    text: &str,
    value: impl Fn(&str) -> Option<String>,
) -> Result<String, Option<String>> {
    let mut rendered = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{") {
        rendered.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("}}").ok_or(None)?;
        let key = after[..end].trim();
        rendered.push_str(&value(key).ok_or_else(|| Some(key.to_owned()))?);
        rest = &after[end + 2..];
    }
    rendered.push_str(rest);
    Ok(rendered)
}
