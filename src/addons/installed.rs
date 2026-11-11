//! An addon installed in a prefix together with the prefix's setting values.

use serde::{Deserialize, Serialize};

use super::{Addon, AddonSettings, SettingValue};
use crate::{PrefixError, error::Result};

/// Pairs a frozen release with the setting values one prefix chose for it.
///
/// Dereferences to the [`Addon`], so release metadata is read directly.
/// Replacing the release keeps every stored value, including values the new
/// release does not declare or accept; [`Self::value`] ignores those.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, derive_more::Deref)]
#[serde(bound(serialize = "R: Serialize", deserialize = "R: Deserialize<'de>"))]
pub struct InstalledAddon<R> {
    /// The frozen release.
    #[deref]
    pub addon: Addon<R>,
    #[serde(default, skip_serializing_if = "AddonSettings::is_empty")]
    settings: AddonSettings,
}

impl<R> InstalledAddon<R> {
    pub(crate) fn new(addon: Addon<R>) -> Self {
        Self {
            addon,
            settings: AddonSettings::new(),
        }
    }

    /// Returns the values the user changed, including values the current
    /// release no longer declares or accepts.
    pub fn settings(&self) -> &AddonSettings {
        &self.settings
    }

    /// Returns the value in effect for setting `key`, or `None` if the release
    /// does not declare it.
    ///
    /// A stored value the setting's control does not accept is ignored in
    /// favor of the default.
    pub fn value(&self, key: &str) -> Option<&SettingValue> {
        let setting = self.addon.settings().get(key)?;
        Some(
            self.settings
                .get(key)
                .filter(|value| setting.control.accepts(value))
                .unwrap_or(&setting.default),
        )
    }

    /// Stores `value` for setting `key`, or removes it when it is the default.
    ///
    /// # Errors
    ///
    /// Returns [`PrefixError::UnknownSetting`] if the release does not declare
    /// `key`, or [`PrefixError::InvalidSettingValue`] if its control does not
    /// accept `value`.
    pub(crate) fn set(&mut self, key: &str, value: SettingValue) -> Result<()> {
        let addon = self.addon.id();
        let setting =
            self.addon
                .settings()
                .get(key)
                .ok_or_else(|| PrefixError::UnknownSetting {
                    addon,
                    key: key.to_owned(),
                })?;
        if !setting.control.accepts(&value) {
            return Err(PrefixError::InvalidSettingValue {
                addon,
                key: key.to_owned(),
                value,
            }
            .into());
        }
        if value == setting.default {
            self.settings.remove(key);
        } else {
            self.settings.insert(key.to_owned(), value);
        }
        Ok(())
    }
}
