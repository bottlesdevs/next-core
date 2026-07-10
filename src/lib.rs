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

    pub mod fvs2d {
        tonic::include_proto!("fvs2d.v1");
    }
}

pub use crate::virgo::{Layer, LayerRevision, VirgoDaemon};
pub use crate::winebridge::{
    DllOverride, DllOverrideMode, Drive, LaunchRequest, PathInfo, PathKind, Process, RegistryHive,
    RegistryKey, RegistryKeyValue, RegistryMultiString, RegistryValue, Service, ServiceStartType,
    ServiceState, WineBridgeClient, WinebootMode,
};
