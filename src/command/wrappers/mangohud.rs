//! MangoHud command wrapper configuration.

use crate::command::{Command, Wrapper};
use serde::{Deserialize, Serialize};

/// Internal MangoHud process wrapper.
pub(crate) struct MangoHud {
    config: MangoHudConfig,
}

impl From<MangoHudConfig> for MangoHud {
    fn from(config: MangoHudConfig) -> Self {
        Self { config }
    }
}

impl Into<Command> for MangoHud {
    fn into(self) -> Command {
        let args = self.config.to_args();
        Command::new("mangohud").args(args).arg("--")
    }
}

impl Wrapper for MangoHud {}

/// Controls whether a launched command is wrapped by MangoHud.
///
/// # Examples
///
/// ```
/// use bottles_core::MangoHudConfig;
///
/// let config = MangoHudConfig { enabled: true };
/// assert!(config.enabled);
/// ```
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct MangoHudConfig {
    /// Whether the `mangohud` executable wraps the launched command.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub enabled: bool,
}

impl MangoHudConfig {
    /// Returns MangoHud arguments derived from this configuration.
    fn to_args(&self) -> Vec<String> {
        Vec::new()
    }
}
