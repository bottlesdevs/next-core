//! Direct mutable prefix storage.
//!
//! Recipes operate on `<owner>/prefix`. Uninstallation asks the recipe to
//! restore overwritten files because, unlike Virgo, this backend has no lower
//! layer to reveal. The software workflow owns mutation rollback.

use std::path::Path;

use crate::{error::Result, runner::Runner};

pub(super) async fn create(prefix: &Path, runner: &dyn Runner) -> Result<()> {
    let result = runner.wineboot(prefix, "--init").await;
    crate::environment::shutdown_wine(runner, prefix).await?;
    result
}
