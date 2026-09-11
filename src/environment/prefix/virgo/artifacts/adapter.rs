//! Runner initialization effects over pinned Soda, with registry changes stored as patches.

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{VirgoLayer, build::build, cache};
use crate::{
    Context,
    error::{Error, Result},
    runner::Runner,
};

pub(crate) async fn prepare_adapter(
    id: Uuid,
    runner: &dyn Runner,
    base: &VirgoLayer,
    cx: &Context,
    cancellation: &CancellationToken,
) -> Result<VirgoLayer> {
    let destination = cx
        .directories()
        .data_dir()
        .join("virgo/adapters")
        .join(id.to_string());
    if let Some(artifact) = cache::load(&destination, Some(id)).await? {
        return Ok(artifact);
    }
    let _build = cancellation
        .run_until_cancelled(cx.artifact_build().lock())
        .await
        .ok_or(Error::Cancelled)?;
    if let Some(artifact) = cache::load(&destination, Some(id)).await? {
        return Ok(artifact);
    }
    build(
        id,
        &destination,
        runner,
        base,
        cx,
        cancellation,
        |prefix, runner| async move { runner.wineboot(&prefix, "--init").await },
    )
    .await
}
