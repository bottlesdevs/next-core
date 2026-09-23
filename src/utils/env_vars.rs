//! Serializable environment-variable overrides.

use std::{borrow::Borrow, collections::HashMap, hash::Hash};

use serde::{Deserialize, Serialize};

/// A map of environment-variable names to replacement values.
///
/// Inserting the same name again replaces its value. Iteration order is not
/// stable because values are stored in a [`HashMap`]. The default key and value
/// type is [`String`]; internal process construction also uses [`OsString`].
///
/// [`OsString`]: std::ffi::OsString
///
/// # Examples
///
/// ```
/// use bottles_core::EnvVars;
///
/// let mut vars = EnvVars::<String>::default();
/// assert_eq!(vars.insert("WINEDEBUG".into(), "-all".into()), None);
/// assert_eq!(vars.get("WINEDEBUG"), Some("-all"));
/// ```
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EnvVars<T: Eq + Hash = String>(HashMap<T, T>);

impl<T: Eq + Hash> EnvVars<T> {
    /// Inserts an override and returns the previous value for `name`, if any.
    pub fn insert(&mut self, name: T, value: T) -> Option<T> {
        self.0.insert(name, value)
    }

    /// Removes and returns the override for `name`, if present.
    ///
    /// The borrowed lookup type may differ from the stored key type, allowing
    /// a string-keyed map to be queried with `&str`.
    pub fn remove<Q>(&mut self, name: &Q) -> Option<T>
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.0.remove(name)
    }

    /// Inserts all supplied overrides, replacing duplicate names.
    pub(crate) fn extend(&mut self, env_vars: impl IntoIterator<Item = (T, T)>) {
        self.0.extend(env_vars);
    }
}

impl EnvVars<String> {
    /// Returns the value assigned to `name`, if present.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    /// Iterates over borrowed `(name, value)` pairs in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Returns `true` when no overrides are present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<T: Eq + Hash> IntoIterator for EnvVars<T> {
    type Item = (T, T);
    type IntoIter = std::collections::hash_map::IntoIter<T, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
