mod error;
pub mod runner;
pub mod download;
pub use error::Error;

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}
