//! Settings a release declares and the values a prefix stores for them.
//!
//! A release freezes its [`AddonSettingsSchema`] when it is acquired. A prefix
//! stores only the values the user changed in [`AddonSettings`]; every other
//! setting reads its default.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Deserializer, Serialize, de};
use thiserror::Error;

/// Values a prefix stores for one addon, keyed by [`Setting::key`].
///
/// Only values that differ from the default are stored. Values are kept when
/// the addon is replaced by another release, even if that release no longer
/// declares or accepts them.
pub type AddonSettings = BTreeMap<String, SettingValue>;

/// Holds the value of one setting.
#[derive(
    Clone, Debug, Deserialize, Eq, PartialEq, Serialize, derive_more::Display, derive_more::From,
)]
#[serde(untagged)]
pub enum SettingValue {
    /// The value of a [`Control::Toggle`].
    Bool(bool),
    /// The value of a [`Control::Range`].
    Integer(i64),
    /// The [`SelectOption::value`] of a [`Control::Select`].
    String(String),
}

/// Groups top-level settings into sections.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, strum::Display)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum Category {
    /// Rendering, upscaling, and GPU selection.
    Graphics,
}

/// Describes how a setting is edited and which values it accepts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Control {
    /// An on or off switch holding a [`SettingValue::Bool`].
    Toggle,
    /// A choice among options holding one option's [`SettingValue::String`].
    Select {
        /// Choices in display order.
        options: Vec<SelectOption>,
    },
    /// A stepped number holding a [`SettingValue::Integer`].
    Range {
        /// Smallest accepted value.
        min: i64,
        /// Largest accepted value.
        max: i64,
        /// Distance between accepted values, counted from `min`.
        #[serde(default = "default_step")]
        step: i64,
    },
}

fn default_step() -> i64 {
    1
}

impl Control {
    /// Returns whether this control can hold `value`.
    pub fn accepts(&self, value: &SettingValue) -> bool {
        match (self, value) {
            (Self::Toggle, SettingValue::Bool(_)) => true,
            (Self::Select { options }, SettingValue::String(value)) => {
                options.iter().any(|option| option.value == *value)
            }
            (Self::Range { min, max, step }, SettingValue::Integer(value)) => {
                (min..=max).contains(&value) && (value - min) % step == 0
            }
            _ => false,
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Toggle => true,
            Self::Select { options } => {
                let mut values = HashSet::new();
                !options.is_empty() && options.iter().all(|option| values.insert(&option.value))
            }
            Self::Range { min, max, step } => min <= max && *step > 0,
        }
    }
}

/// One choice of a [`Control::Select`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    /// Value stored and used by the addon's effects.
    pub value: String,
    /// Text shown to the user.
    pub label: String,
}

/// Declares one setting of a release.
///
/// A top-level setting has a category and may hold child settings shown with
/// it. Children have no category or children of their own.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    /// Identifies the setting among every setting of its release.
    pub key: String,
    /// Name shown to the user.
    pub label: String,
    /// Optional explanation shown below the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Section holding a top-level setting; `None` for children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<Category>,
    /// Whether the setting is hidden behind an advanced view.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub advanced: bool,
    /// How the setting is edited.
    pub control: Control,
    /// Value used until the user changes it.
    pub default: SettingValue,
    /// Settings shown with this one, in display order.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "children"
    )]
    pub children: Vec<Setting>,
}

fn children<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Setting>, D::Error> {
    let children = Vec::<Setting>::deserialize(deserializer)?;
    if let Some(child) = children
        .iter()
        .find(|child| child.category.is_some() || !child.children.is_empty())
    {
        return Err(de::Error::custom(format!(
            "child setting `{}` cannot have a category or children",
            child.key
        )));
    }
    Ok(children)
}

/// Holds the settings a release declares, in display order.
///
/// Deserialization validates the schema: keys are unique across both levels,
/// top-level settings have a category, controls are well formed, and every
/// default is accepted by its control.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "Vec<Setting>", into = "Vec<Setting>")]
pub struct AddonSettingsSchema(Vec<Setting>);

impl AddonSettingsSchema {
    /// Returns whether the release declares no settings.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the top-level settings in display order.
    pub fn iter(&self) -> impl Iterator<Item = &Setting> {
        self.0.iter()
    }

    /// Returns the top-level or child setting with `key`, if declared.
    pub fn get(&self, key: &str) -> Option<&Setting> {
        all(&self.0).find(|setting| setting.key == key)
    }
}

/// Lists `settings` and their children.
fn all(settings: &[Setting]) -> impl Iterator<Item = &Setting> {
    settings
        .iter()
        .flat_map(|setting| std::iter::once(setting).chain(&setting.children))
}

impl From<AddonSettingsSchema> for Vec<Setting> {
    fn from(settings: AddonSettingsSchema) -> Self {
        settings.0
    }
}

impl TryFrom<Vec<Setting>> for AddonSettingsSchema {
    type Error = SchemaError;

    fn try_from(settings: Vec<Setting>) -> Result<Self, Self::Error> {
        if let Some(setting) = settings.iter().find(|setting| setting.category.is_none()) {
            return Err(SchemaError::MissingCategory(setting.key.clone()));
        }
        let mut keys = HashSet::new();
        for setting in all(&settings) {
            if !keys.insert(&setting.key) {
                return Err(SchemaError::DuplicateKey(setting.key.clone()));
            }
            if !setting.control.is_valid() {
                return Err(SchemaError::InvalidControl(setting.key.clone()));
            }
            if !setting.control.accepts(&setting.default) {
                return Err(SchemaError::InvalidDefault(setting.key.clone()));
            }
        }
        Ok(Self(settings))
    }
}

/// Reports a release settings schema that cannot be used.
#[derive(Debug, Error)]
pub enum SchemaError {
    /// Two settings share a key.
    #[error("setting `{0}` is declared more than once")]
    DuplicateKey(String),
    /// A select has no options or repeats a value, or a range is empty or has
    /// no positive step.
    #[error("setting `{0}` has an invalid control")]
    InvalidControl(String),
    /// The default is not accepted by the setting's control.
    #[error("setting `{0}` has a default its control does not accept")]
    InvalidDefault(String),
    /// A top-level setting has no category.
    #[error("setting `{0}` has no category")]
    MissingCategory(String),
}
