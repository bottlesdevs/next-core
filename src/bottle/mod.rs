#[allow(clippy::module_inception)]
mod bottle;
mod error;
mod manager;
mod virgo;

#[cfg(test)]
mod tests;

pub use bottle::{Bottle, BottleComponents, BottleType, Program};
pub(crate) use bottle::{PrefixStorage, invalid_components};
pub use error::BottleError;
pub(crate) use manager::fvs;
pub use manager::{BottleManager, BottleManagerConfig};
