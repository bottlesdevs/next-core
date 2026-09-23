//! Serializable configuration for optional launch-time process wrappers.
//!
//! [`Wrappers`] applies gamescope and MangoHud without double wrapping: when
//! both are enabled, gamescope's native `--mangoapp` integration is used.

pub(crate) mod gamescope;
pub(crate) mod mangohud;

use serde::{Deserialize, Serialize};

use crate::runner::RunnerCommand;

pub use gamescope::{Filter as GamescopeFilter, GamescopeConfig, Scaler as GamescopeScaler};
pub use mangohud::MangoHudConfig;

use self::{gamescope::Gamescope, mangohud::MangoHud};

/// Process wrappers applied to a launched program.
///
/// Disabled configurations are retained so callers can toggle a wrapper
/// without discarding its other settings.
///
/// # Examples
///
/// ```
/// use bottles_core::{GamescopeConfig, Wrappers};
///
/// let wrappers = Wrappers {
///     gamescope: GamescopeConfig {
///         enabled: true,
///         fullscreen: true,
///         ..GamescopeConfig::default()
///     },
///     ..Wrappers::default()
/// };
/// assert!(wrappers.gamescope.enabled);
/// ```
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct Wrappers {
    /// Gamescope wrapper configuration.
    #[serde(default)]
    pub gamescope: GamescopeConfig,
    /// MangoHud wrapper configuration.
    #[serde(default)]
    pub mangohud: MangoHudConfig,
}

impl Wrappers {
    /// Applies enabled wrappers to `command`.
    ///
    /// When both wrappers are enabled, this enables gamescope's MangoApp
    /// support instead of nesting a separate `mangohud` process.
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
