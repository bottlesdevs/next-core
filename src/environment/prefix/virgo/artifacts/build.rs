//! Execute Wine work and stop it before handing the workspace back to storage.

use std::{future::Future, path::PathBuf};

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    environment::runtime,
    error::{Error, Result},
    runner::Runner,
    virgo::{LayerBuild, VirgoLayer},
};

pub(super) async fn run<'a, Fut>(
    mut build: LayerBuild<'_>,
    id: Uuid,
    message: String,
    runner: &'a dyn Runner,
    base: Option<&VirgoLayer>,
    cancellation: &CancellationToken,
    work: impl FnOnce(PathBuf, &'a dyn Runner) -> Fut + Send,
) -> Result<VirgoLayer>
where
    Fut: Future<Output = Result<()>> + Send,
{
    let prefix = match build.prepare(base, cancellation).await {
        Ok(prefix) => prefix,
        Err(error) => {
            build.discard().await?;
            return Err(error);
        }
    };
    let executed = if cancellation.is_cancelled() {
        Err(Error::Cancelled)
    } else {
        work(prefix.clone(), runner).await
    };
    // A failed shutdown drops the reservation while retaining staging and any mount.
    runtime::stop(runner, &prefix).await?;
    if let Err(error) = executed {
        build.discard().await?;
        return Err(error);
    }
    build.finish(id, message, cancellation).await
}
