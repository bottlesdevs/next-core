pub(crate) mod gamescope;
pub(crate) mod mangohud;

use serde::{Deserialize, Serialize};

use crate::runner::RunnerCommand;

pub use gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler};
pub use mangohud::MangoHudConfig;

use self::{gamescope::Gamescope, mangohud::MangoHud};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Wrappers {
    #[serde(default)]
    pub gamescope: GamescopeConfig,
    #[serde(default)]
    pub mangohud: MangoHudConfig,
}

impl Wrappers {
    pub(crate) fn apply(&self, command: RunnerCommand) -> RunnerCommand {
        match (self.gamescope.enabled, self.mangohud.enabled) {
            (false, false) => command,
            (false, true) => command.wrapped_by(MangoHud::from(self.mangohud.clone())),
            (true, false) => command.wrapped_by(Gamescope::from(self.gamescope.clone())),
            (true, true) => {
                command.wrapped_by(Gamescope::from(self.gamescope.clone()).with_mangoapp())
            }
        }
    }
}
