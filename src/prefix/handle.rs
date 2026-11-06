//! The public handle to one persisted prefix.

use super::{Edit, Inner, PrefixState};
use crate::{
    Operation, ProgramSpec,
    error::Result,
    proto::{DllOverride, DllOverrideMode, Process},
};
use futures_core::Stream;
use std::sync::Arc;
use uuid::Uuid;

/// A live handle to one persisted Wine prefix.
///
/// Clones share state and operation coordination. Dropping the last handle does
/// not stop Wine; call [`Prefix::stop`] when shutdown is required. A handle
/// kept after [`Prefixes::delete`](crate::Prefixes::delete) reports
/// [`PrefixError::Deleted`](crate::PrefixError::Deleted).
#[derive(Clone)]
pub struct Prefix(pub(crate) Arc<Inner>);

impl Prefix {
    /// Returns the prefix's persistent identifier.
    pub fn id(&self) -> Uuid {
        self.0.id
    }

    /// Returns the currently published immutable state snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::Deleted`](crate::PrefixError::Deleted)
    /// after the prefix is deleted.
    pub fn state(&self) -> Result<Arc<PrefixState>> {
        self.0.state()
    }

    /// Streams the current state and later published replacements.
    ///
    /// The first item is the state current at subscription time. Slow consumers
    /// may miss intermediate states, and deletion ends the stream.
    pub fn watch(&self) -> impl Stream<Item = Arc<PrefixState>> + Send + 'static + use<> {
        self.0.watch()
    }

    /// Creates an operation that edits a private draft of the latest state.
    ///
    /// The final draft is validated, applied, saved, and then published once.
    /// Software selection changes require a stopped prefix; startup
    /// settings may change while Wine is running. An unchanged draft is not saved.
    /// Any shutdown begun for the edit finishes before cancellation is returned.
    /// Once application succeeds, persistence and publication are not cancelled.
    /// Standard-prefix software edits are not rolled back: failure or cancellation
    /// can leave files changed while the previous state remains published. Virgo
    /// failures after checkpoint capture recover the previous files before returning.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns errors from the callback, validation,
    /// cancellation, persistence, software application, or storage recovery.
    /// [`PrefixError::MustBeStopped`](crate::PrefixError::MustBeStopped)
    /// is returned when software changes are requested while Wine is running.
    ///
    /// # Examples
    ///
    /// ```
    /// # use bottles_core::Prefix;
    /// # fn prepare(prefix: &Prefix) {
    /// let quiet = prefix.edit(|edit| {
    ///     edit.env_vars().insert("WINEDEBUG".into(), "-all".into());
    ///     Ok(())
    /// });
    /// # let _ = quiet;
    /// # }
    /// ```
    pub fn edit<R: Send + 'static>(
        &self,
        callback: impl FnOnce(&mut Edit<'_>) -> Result<R> + Send + 'static,
    ) -> Operation<R> {
        self.0.edit(callback)
    }

    /// Creates an operation that launches `program` in process group `group`.
    ///
    /// `group` is the `WineBridge` process-group identifier chosen by the
    /// caller; it is not persisted. Success returns the new process ID; it does
    /// not wait for the process to exit. Cancellation does not interrupt an
    /// in-flight launch request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation may return prefix, runner, storage,
    /// cancellation, or `WineBridge` errors while starting the process.
    pub fn launch(&self, group: Uuid, program: ProgramSpec) -> Operation<u32> {
        self.0.launch(group, program)
    }

    /// Terminates a `WineBridge` process group without starting a stopped runtime.
    ///
    /// A stopped runtime is a successful no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, discovery fails, or
    /// `WineBridge` cannot terminate the process group.
    pub async fn kill(&self, group: Uuid) -> Result<()> {
        self.0.kill(group).await
    }

    /// Lists processes currently reported by `WineBridge`.
    ///
    /// A stopped prefix returns an empty vector and is not started.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, discovery fails, or the
    /// connected `WineBridge` cannot list its processes.
    pub async fn processes(&self) -> Result<Vec<Process>> {
        self.0.processes().await
    }

    /// Stops Wine and releases storage mounted for the prefix.
    ///
    /// Calling this for an already stopped prefix is safe.
    ///
    /// # Errors
    ///
    /// Returns an error if state or runner resolution, Wine shutdown, storage
    /// release, or discovery cleanup fails.
    pub async fn stop(&self) -> Result<()> {
        self.0.stop().await
    }

    /// Creates an operation that lists configured DLL overrides.
    ///
    /// The operation starts or reconnects to `WineBridge` if necessary and
    /// leaves the runtime running. Cancellation does not interrupt an in-flight
    /// `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn dll_overrides(&self) -> Operation<Vec<DllOverride>> {
        self.0.dll_overrides()
    }

    /// Creates an operation that sets one Wine DLL override.
    ///
    /// A stopped runtime is started and left running. Cancellation does not
    /// interrupt an in-flight `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn set_dll_override(&self, dll: impl Into<String>, mode: DllOverrideMode) -> Operation<()> {
        self.0.set_dll_override(dll.into(), mode)
    }

    /// Creates an operation that removes one Wine DLL override.
    ///
    /// A stopped runtime is started and left running. A missing override is a
    /// successful no-op. Cancellation does not interrupt an in-flight
    /// `WineBridge` request.
    ///
    /// # Errors
    ///
    /// Awaiting the operation can fail during prefix access, runner or
    /// `WineBridge` startup, cancellation, or the `WineBridge` request.
    pub fn unset_dll_override(&self, dll: impl Into<String>) -> Operation<()> {
        self.0.unset_dll_override(dll.into())
    }
}

#[cfg(feature = "fvs")]
impl Prefix {
    /// Creates an operation that captures the prefix's managed state.
    ///
    /// Shared artifacts and external files are not copied or rebuilt on
    /// restoration. Explicit snapshots create a revision even without changes.
    /// The prefix is stopped before capture and is not restarted afterward.
    /// Shutdown and capture are not interrupted by cancellation;
    /// [`Operation::cancel`] waits for the current step to finish.
    ///
    /// # Errors
    ///
    /// Awaiting the operation returns an invalid-input I/O error when `message`
    /// is the reserved internal checkpoint message. It can also fail during
    /// cancellation, Wine shutdown, repository initialization, or commit.
    pub fn create_snapshot(&self, message: impl Into<String>) -> Operation<crate::Snapshot> {
        self.0.create_snapshot(message.into())
    }

    /// Lists user snapshots from newest to oldest.
    ///
    /// Internal checkpoints are excluded. A prefix with no history
    /// returns an empty vector without contacting FVS.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix was deleted, history metadata cannot
    /// be read, or FVS cannot list revisions.
    pub async fn snapshots(&self) -> Result<Vec<crate::SnapshotSummary>> {
        self.0.snapshots().await
    }

    /// Creates an operation that restores a snapshot into the managed root.
    ///
    /// A failed restore or unreadable restored state recovers the previous files
    /// before returning; failed recovery reports the root requiring repair.
    /// Shutdown and checkpoint capture finish before cancellation is observed.
    /// Once restoration begins, cancellation does not interrupt restoration or
    /// recovery. Working files change without moving the FVS repository's
    /// current commit. Rollback first attempts to stop the prefix and never
    /// restarts it.
    ///
    /// On success, the operation returns the full state ID resolved from
    /// `revision` and publishes the state stored in that revision.
    ///
    /// # Errors
    ///
    /// Awaiting the operation fails on cancellation before restoration, Wine
    /// shutdown or checkpoint failure, an unknown revision, mismatched or
    /// invalid restored state, or failed restoration or recovery.
    pub fn rollback(&self, revision: &str) -> Operation<String> {
        self.0.rollback(revision)
    }
}
