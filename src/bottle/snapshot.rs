//! Bottle-root history using shared environment orchestration.
use super::Bottle;
use crate::{Operation, Snapshot, SnapshotSummary, error::Result};

impl Bottle {
    /// Stop the environment and capture its managed files and `bottle.toml`.
    /// Pending Virgo selections stay pending; shared artifacts and external files
    /// are not copied. Explicit snapshots create a revision even without changes.
    /// The internal checkpoint message is reserved. Once capture starts it finishes
    /// under coordination, including when explicit cancellation is requested.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<Snapshot> {
        self.0.create_snapshot(message.into())
    }

    /// List newest-first user snapshots, excluding internal checkpoints.
    /// A bottle without history returns an empty list without contacting FVS.
    pub async fn snapshots(&self) -> Result<Vec<SnapshotSummary>> {
        self.0.snapshots().await
    }

    /// Restore the complete managed root and publish its restored BottleState.
    /// UUID, backend and configuration format must match. A failed restore or
    /// invalid state recovers the previous files before returning; failed recovery
    /// reports the root requiring repair. Cancellation does not interrupt an active
    /// restore or recovery. Working files change without moving FVS's current commit.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}
