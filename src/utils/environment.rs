use std::{borrow::Borrow, collections::HashMap, hash::Hash};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Environment<T: Eq + Hash = String>(HashMap<T, T>);

impl<T: Eq + Hash> Environment<T> {
    pub(crate) fn insert(&mut self, name: T, value: T) -> Option<T> {
        self.0.insert(name, value)
    }

    pub(crate) fn remove<Q>(&mut self, name: &Q) -> Option<T>
    where
        T: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.0.remove(name)
    }

    pub(crate) fn extend(&mut self, environment: impl IntoIterator<Item = (T, T)>) {
        self.0.extend(environment);
    }
}

impl Environment<String> {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<T: Eq + Hash> IntoIterator for Environment<T> {
    type Item = (T, T);
    type IntoIter = std::collections::hash_map::IntoIter<T, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
