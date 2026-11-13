//! Catalog version strings and their ordering.

use std::{cmp::Ordering, fmt};

use serde::{Deserialize, Serialize};

/// A release's catalog-provided version, ordered across catalog formats.
///
/// Catalogs do not share one version format, so a version is split into runs
/// of ASCII digits and runs of other characters. Digit runs compare as numbers
/// and other runs as text, text before numbers, and a version that is a prefix
/// of another comes first. `11.15` follows `11.9`, `11-5` follows `10-30`, and
/// `11.0.4` follows `11.0`. Versions that differ only in leading zeros, such as
/// `1.01` and `1.1`, are ordered by their text so that ordering agrees with
/// equality.
///
/// Versions of different series, such as Wine and Proton-GE, do not compare
/// meaningfully. Prerelease suffixes are not recognized: `11.0-rc1` follows
/// `11.0`.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Version(String);

impl Version {
    /// Wraps a catalog version string.
    pub fn new(version: impl Into<String>) -> Self {
        Self(version.into())
    }

    /// Returns the version as written by its catalog.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let (mut left, mut right) = (runs(&self.0), runs(&other.0));
        loop {
            let ordering = match (left.next(), right.next()) {
                (None, None) => return self.0.cmp(&other.0),
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(left), Some(right)) => compare_runs(left, right),
            };
            if ordering.is_ne() {
                return ordering;
            }
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Splits `version` where digits meet other characters.
fn runs(version: &str) -> impl Iterator<Item = &[u8]> {
    version
        .as_bytes()
        .chunk_by(|left, right| left.is_ascii_digit() == right.is_ascii_digit())
}

/// Compares two runs, ordering text before numbers.
fn compare_runs(left: &[u8], right: &[u8]) -> Ordering {
    match (number(left), number(right)) {
        (Some(left), Some(right)) => left.len().cmp(&right.len()).then_with(|| left.cmp(right)),
        (None, None) => left.cmp(right),
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
    }
}

/// Returns the digits of a digit run without leading zeros.
fn number(run: &[u8]) -> Option<&[u8]> {
    run.first()?.is_ascii_digit().then(|| {
        let zeros = run.iter().take_while(|digit| **digit == b'0').count();
        &run[zeros..]
    })
}
