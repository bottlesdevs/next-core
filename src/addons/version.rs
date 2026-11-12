//! Ordering of catalog version strings.

use std::cmp::Ordering;

/// Compares two version strings of the same release series.
///
/// Catalogs do not share one version format, so each string is split into
/// runs of ASCII digits and runs of other characters. Digit runs compare as
/// numbers and other runs as text, and a version that is a prefix of another
/// comes first. `11.15` follows `11.9`, `11-5` follows `10-30`, and `11.0.4`
/// follows `11.0`.
///
/// Versions of different series, such as Wine and Proton-GE, do not compare
/// meaningfully. Prerelease suffixes are not recognized: `11.0-rc1` follows
/// `11.0`.
pub fn compare_versions(left: &str, right: &str) -> Ordering {
    let (mut left, mut right) = (runs(left), runs(right));
    loop {
        let ordering = match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left), Some(right)) => compare_runs(left, right),
        };
        if ordering.is_ne() {
            return ordering;
        }
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
