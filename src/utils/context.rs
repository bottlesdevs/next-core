//! Shared services used by one core instance.

use crate::{Addons, error::Result};
use std::sync::Arc;

struct ContextInner {
    addons: Addons,
    #[cfg(feature = "fvs")]
    fvs: crate::virgo::Fvs,
}

#[derive(Clone)]
pub(crate) struct Context(Arc<ContextInner>);

impl Context {
    /// Constructs shared services and loads locally acquired releases.
    ///
    /// # Errors
    ///
    /// Returns an error if a release manifest cannot be loaded from storage.
    pub(crate) async fn new(#[cfg(feature = "fvs")] fvs: crate::virgo::Fvs) -> Result<Self> {
        Ok(Self(Arc::new(ContextInner {
            addons: Addons::load().await?,
            #[cfg(feature = "fvs")]
            fvs,
        })))
    }

    pub(crate) fn addons(&self) -> &Addons {
        &self.0.addons
    }

    #[cfg(feature = "fvs")]
    pub(crate) fn fvs(&self) -> &crate::virgo::Fvs {
        &self.0.fvs
    }
}
