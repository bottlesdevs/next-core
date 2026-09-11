//! Validate edited execution selections before lifecycle or prefix work.

use strum::IntoEnumIterator;

use super::{EnvironmentConfig, EnvironmentError};
use crate::{Addon, AddonError, Addons, Slot, error::Result};

/// Validate selections before lifecycle or prefix work.
pub(crate) fn validate_edit(
    previous: &EnvironmentConfig,
    candidate: &EnvironmentConfig,
    addons: &Addons,
) -> Result<()> {
    if candidate.storage != previous.storage {
        return Err(EnvironmentError::InvalidEdit("storage strategy is fixed at creation").into());
    }
    super::prefix::validate_edit(previous, candidate)?;

    for slot in Slot::iter() {
        let old = previous.component(slot);
        let new = candidate.component(slot);
        if old == new {
            continue;
        }
        if let Some(new) = new {
            let downloaded = addons
                .component(new.id())
                .ok_or(AddonError::NotFound(new.id()))?;
            if Addon::from(downloaded.as_ref()) != *new {
                return Err(EnvironmentError::InvalidEdit(
                    "component selection must match its downloaded release",
                )
                .into());
            }
        }
    }
    for new in &candidate.dependencies {
        if candidate
            .dependencies
            .iter()
            .filter(|addon| addon.id() == new.id())
            .count()
            != 1
        {
            return Err(
                EnvironmentError::InvalidEdit("a dependency may only be selected once").into(),
            );
        }
        if previous.dependency(new.id()) == Some(new) {
            continue;
        }
        let downloaded = addons
            .dependency(new.id())
            .ok_or(AddonError::NotFound(new.id()))?;
        if Addon::from(downloaded.as_ref()) != *new {
            return Err(EnvironmentError::InvalidEdit(
                "dependency selection must match its downloaded release",
            )
            .into());
        }
    }

    Ok(())
}
