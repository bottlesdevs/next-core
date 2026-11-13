//! Errors raised while loading, validating, or mutating prefixes.

use crate::{PrefixBackend, Requirement, SettingValue, Slot, WineArch};
use thiserror::Error;
use uuid::Uuid;

/// Describes invalid prefix state and operations rejected by its lifecycle.
///
/// I/O, runner, addon, `WineBridge`, and Virgo failures are represented by other
/// variants of [`crate::error::Error`].
#[derive(Debug, Error)]
pub enum PrefixError {
    /// The collection has no prefix with the requested identifier.
    #[error("prefix {0} was not found")]
    NotFound(Uuid),
    /// The prefix was deleted while a handle to it remained alive.
    #[error("prefix was deleted")]
    Deleted,
    /// Restoring the automatic checkpoint after a failed mutation also failed.
    #[error("rollback failed for {root}; repair or restore this prefix before retrying: {source}")]
    Rollback {
        /// Root directory of the prefix that could not be recovered.
        root: std::path::PathBuf,
        /// Error returned while restoring the checkpoint.
        #[source]
        source: Box<crate::error::Error>,
    },

    /// A program or the user refused to end the session, so the prefix keeps
    /// running.
    ///
    /// Programs asked before the refusal may already have been closed. A
    /// refusing program may ask the user afterwards whether to close; the
    /// stop is not resumed when it does.
    #[error("a program refused to close; answer its prompt, then stop the prefix again")]
    StopRefused,
    /// Software selections cannot be changed while the prefix is running.
    #[error("stop the prefix before changing its settings")]
    MustBeStopped,
    /// No selected component occupies the requested [`Slot`].
    #[error("component slot {0:?} is not installed")]
    ComponentNotInstalled(Slot),
    /// The prefix has no runner, component, or dependency with this release UUID.
    #[error("addon {0} is not selected in this prefix")]
    AddonNotSelected(Uuid),
    /// The selected release does not declare the requested setting.
    #[error("addon {addon} has no setting `{key}`")]
    UnknownSetting {
        /// Release UUID of the addon.
        addon: Uuid,
        /// Requested setting key.
        key: String,
    },
    /// The setting's control does not accept the value.
    #[error("setting `{key}` of addon {addon} does not accept {value}")]
    InvalidSettingValue {
        /// Release UUID of the addon.
        addon: Uuid,
        /// Setting key.
        key: String,
        /// Rejected value.
        value: SettingValue,
    },
    /// This build cannot create, start, or change prefixes of the backend.
    #[error("the {0} backend is not available in this build")]
    BackendUnavailable(PrefixBackend),
    /// The runner or backend cannot hold a prefix of this architecture.
    #[error("{by} does not support {arch} prefixes")]
    UnsupportedArch {
        /// Rejected architecture.
        arch: WineArch,
        /// What rejects it.
        by: ArchLimit,
    },
    /// One or more addon requirements are absent from the proposed configuration.
    #[error("addon requirements are not satisfied: {requirements:?}")]
    RequiresAddon {
        /// Addon requesting the requirements.
        required_by: Uuid,
        /// Requirements not satisfied by any selected addon.
        requirements: Vec<Requirement>,
    },
}

/// What limits the architectures a prefix can use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, strum::Display)]
pub enum ArchLimit {
    /// Proton runners, which run only 64-bit prefixes.
    #[strum(serialize = "Proton")]
    Proton,
    /// Virgo storage, whose shared layers are built as 64-bit prefixes.
    #[strum(serialize = "Virgo storage")]
    Virgo,
}
