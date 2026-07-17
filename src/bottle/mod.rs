#[allow(clippy::module_inception)]
mod bottle;
pub(crate) mod error;
mod manager;
mod virgo;

#[cfg(test)]
mod tests;

pub use bottle::{Bottle, BottleType, Program};
pub use manager::BottleManager;
