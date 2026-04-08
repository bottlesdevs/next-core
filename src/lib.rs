mod error;
pub mod runner;
pub mod bottle;
pub mod persistence;
pub mod diff;
pub use error::Error;

pub mod proto {
    pub mod bottles {
         tonic::include_proto!("bottles");
    }
    pub mod winebridge {
         tonic::include_proto!("winebridge");
    }
}
