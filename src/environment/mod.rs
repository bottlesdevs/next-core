//! Shared state, persistence and execution workflows for bottles and standalone programs.

mod config;
mod edit;
mod error;
#[cfg(feature = "fvs")]
pub(crate) mod history;
mod operations;
mod prefix;
mod registry;
mod runtime;
pub(crate) use registry::Registry;
#[cfg(feature = "fvs")]
mod snapshot;
mod state;
#[cfg(feature = "fvs")]
pub use snapshot::{Snapshot, SnapshotSummary};

pub use config::EnvironmentState;
pub use edit::Edit;
pub use error::EnvironmentError;
pub use prefix::PrefixBackend;
#[cfg(feature = "fvs")]
pub use prefix::VirgoError;
#[cfg(feature = "fvs")]
pub(crate) use prefix::VirgoManager;
pub(crate) use state::{Environment, EnvironmentOwnerState};
