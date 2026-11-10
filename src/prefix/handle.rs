//! The public handle to one persisted prefix.

use super::Inner;
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
}
