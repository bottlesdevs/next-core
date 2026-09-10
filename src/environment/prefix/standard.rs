//! Direct mutable prefix storage.
//!
//! Recipes operate on `<owner>/prefix`. Uninstallation asks the recipe to
//! restore overwritten files because, unlike Virgo, this backend has no lower
//! layer to reveal. Transaction rollback is provided by the parent module.

use std::{ops::AsyncFnOnce, path::Path};

use crate::{
    error::Result,
    runner::{Runner, initialize_and_shutdown_prefix},
};

pub(super) async fn create(prefix: &Path, runner: &dyn Runner) -> Result<()> {
    initialize_and_shutdown_prefix(runner, prefix).await
}

pub(super) async fn install<F>(prefix: &Path, execute: F) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path) -> Result<()>,
{
    execute(prefix).await
}

pub(super) async fn uninstall<F>(prefix: &Path, execute: F) -> Result<()>
where
    F: for<'a> AsyncFnOnce(&'a Path, bool) -> Result<()>,
{
    execute(prefix, true).await
}
