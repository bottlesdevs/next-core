use crate::runner::{Command, Wrapper};
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct MangoHudConfig {
    pub(crate) enabled: bool,
}

impl MangoHudConfig {
    fn to_args(&self) -> Vec<String> {
        Vec::new()
    }
}
