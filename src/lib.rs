mod error;
pub mod layers;
pub mod runner;
pub use error::Error;
mod utils;
mod winebridge;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}

pub use crate::winebridge::{
    LaunchRequest, RegistryHive, RegistryMultiString, RegistryValue, WineBridgeClient,
};
