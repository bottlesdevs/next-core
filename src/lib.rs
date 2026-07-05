mod error;
pub mod runner;
pub use error::Error;
mod utils;
mod winebridge;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}

pub use crate::winebridge::{
    LaunchRequest, RegistryHive, RegistryKey, RegistryKeyValue, RegistryMultiString, RegistryValue,
    WineBridgeClient,
};
