//! Standard prefix storage.

use std::{ops::AsyncFnOnce, path::Path};

use crate::{
    error::Result,
    runner::{Runner, initialize_and_shutdown_prefix},
};

pub(super) async fn create(bottle_path: &Path, runner: &dyn Runner) -> Result<()> {
    initialize_and_shutdown_prefix(runner, &bottle_path.join("prefix")).await
}

pub(super) async fn install<F>(bottle_path: &Path, execute: F) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    execute(&bottle_path.join("prefix")).await
}

pub(super) async fn uninstall<F>(bottle_path: &Path, execute: F) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path, bool) -> Result<()>,
{
    execute(&bottle_path.join("prefix"), true).await
}
