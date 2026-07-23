pub mod bottle;
pub mod compatibility;
pub mod error;
mod runner;
mod utils;
mod winebridge;
mod wrapper;

pub use runner::RunnerKind;
pub use utils::{context::Context, directories::Directories, environment::Environment};

pub mod proto {
    tonic::include_proto!("winebridge");
    tonic::include_proto!("bottles");
}
