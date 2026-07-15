pub mod catalog;
mod error;
pub mod layers;
pub mod runner;
pub use error::Error;
mod utils;
pub mod virgo;
mod winebridge;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}

pub use crate::winebridge::{LaunchRequest, WineBridgeClient};
