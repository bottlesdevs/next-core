mod error;
pub mod runner;
pub use error::Error;
mod winebridge;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}
