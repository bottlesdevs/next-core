#[allow(clippy::module_inception)]
mod bottle;
mod error;
mod manager;
mod virgo;

#[cfg(test)]
mod tests;

pub use bottle::{Bottle, BottleComponents, BottleType, Program};
pub use error::BottleError;
pub use manager::{BottleManager, BottleManagerConfig};
