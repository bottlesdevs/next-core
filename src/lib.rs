mod error;
pub use error::Error;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}

#[cfg(feature = "winebridge")]
pub mod winebridge;

#[cfg(feature = "server")]
pub mod server;
