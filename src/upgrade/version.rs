//! Minimal `MAJOR.MINOR.PATCH` version handling for release tags.
//!
//! The workspace does not depend on the `semver` crate directly (it is only a
//! transitive dependency in `Cargo.lock`), so per the design we compare three
//! numeric segments by hand instead of pulling a new dependency.

use std::fmt;
use std::str::FromStr;

use super::error::{Result, UpgradeError};

/// A parsed `MAJOR.MINOR.PATCH` version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    pub fn major(self) -> u32 {
        self.major
    }

    pub fn minor(self) -> u32 {
        self.minor
    }

    pub fn patch(self) -> u32 {
        self.patch
    }

    /// Parse a strict `M.m.p` string: digits only, no `v` prefix, no
    /// prerelease (`-rc1`) or build (`+meta`) suffix, no extra segments.
    pub fn parse(s: &str) -> Result<Self> {
        parse_parts(s).ok_or_else(|| UpgradeError::invalid_data(format!("invalid version: {s:?}")))
    }

    /// Parse a release tag such as `v0.9.0` or `0.9.0`.
    ///
    /// Returns `None` for prereleases (`v0.9.0-rc1`), partial tags (`v0.9`),
    /// non-numeric tags (`latest`), or anything with more than three segments.
    pub fn parse_release_tag(tag: &str) -> Option<Self> {
        let stripped = tag.strip_prefix('v').unwrap_or(tag);
        parse_parts(stripped)
    }
}

/// `true` when `tag` matches the stable release shape `^v?\d+\.\d+\.\d+$`.
pub fn is_stable_release_tag(tag: &str) -> bool {
    Version::parse_release_tag(tag).is_some()
}

fn parse_parts(s: &str) -> Option<Version> {
    let mut segments = s.split('.');
    let major = segments.next()?;
    let minor = segments.next()?;
    let patch = segments.next()?;
    // Any extra segment (e.g. `0.9.0.1`) disqualifies the tag.
    if segments.next().is_some() {
        return None;
    }
    Some(Version {
        major: major.parse().ok()?,
        minor: minor.parse().ok()?,
        patch: patch.parse().ok()?,
    })
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = UpgradeError;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_v_prefixed() {
        assert_eq!(Version::parse("0.9.0").unwrap(), Version::new(0, 9, 0));
        assert_eq!(Version::parse_release_tag("v0.9.0").unwrap(), Version::new(0, 9, 0));
        assert_eq!(Version::parse_release_tag("0.9.0").unwrap(), Version::new(0, 9, 0));
        assert_eq!(Version::parse("10.2.3").unwrap().major(), 10);
    }

    #[test]
    fn rejects_malformed_tags() {
        for bad in [
            "",
            "v",
            "0.9",
            "0.9.0.1",
            "0.9.0-rc1",
            "v0.9.0-beta",
            "0.9.a",
            "0..0",
            "x.y.z",
            "latest",
            "1.2",
            "1",
            "1.2.3.4",
        ] {
            assert!(Version::parse(bad).is_err(), "parse should reject {bad:?}");
            assert!(Version::parse_release_tag(bad).is_none(), "tag should reject {bad:?}");
        }
    }

    #[test]
    fn stable_tag_filter() {
        assert!(is_stable_release_tag("v0.9.0"));
        assert!(is_stable_release_tag("0.9.0"));
        assert!(!is_stable_release_tag("v0.9.0-rc1"));
        assert!(!is_stable_release_tag("v0.9.0.1"));
        assert!(!is_stable_release_tag("latest"));
        assert!(!is_stable_release_tag("v0.9"));
        assert!(!is_stable_release_tag("v0.9.0-alpha"));
    }

    #[test]
    fn numeric_ordering_not_lexicographic() {
        assert!(Version::new(0, 9, 0) > Version::new(0, 8, 2));
        assert!(Version::new(0, 10, 0) > Version::new(0, 9, 9), "0.10.0 must beat 0.9.9");
        assert!(Version::new(1, 0, 0) > Version::new(0, 99, 99));
        assert!(Version::new(0, 9, 0) <= Version::new(0, 9, 0));
        assert_eq!(
            Version::new(0, 9, 0).cmp(&Version::new(0, 9, 0)),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn display_round_trips() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(v.to_string(), "1.2.3");
        assert_eq!(v.to_string().parse::<Version>().unwrap(), v);
    }

    #[test]
    fn version_new_exposes_parts() {
        let v = Version::new(2, 4, 6);
        assert_eq!(v.major(), 2);
        assert_eq!(v.minor(), 4);
        assert_eq!(v.patch(), 6);
    }

    #[test]
    fn version_ordering_across_patch_minor_major() {
        assert!(Version::new(0, 9, 0) > Version::new(0, 8, 99));
        assert!(Version::new(1, 0, 0) > Version::new(0, 99, 99));
        assert!(Version::new(0, 0, 1) < Version::new(0, 0, 2));
        assert_eq!(
            Version::new(1, 2, 3).cmp(&Version::new(1, 2, 3)),
            std::cmp::Ordering::Equal
        );
    }

    #[test]
    fn parse_release_tag_variants() {
        assert_eq!(Version::parse_release_tag("v1.0.0"), Some(Version::new(1, 0, 0)));
        assert_eq!(Version::parse_release_tag("1.0.0"), Some(Version::new(1, 0, 0)));
        assert_eq!(Version::parse_release_tag("v10.20.30"), Some(Version::new(10, 20, 30)));
        assert_eq!(
            Version::parse_release_tag("V1.0.0"),
            None,
            "uppercase V is not accepted"
        );
        assert_eq!(Version::parse_release_tag("v1.0.0+meta"), None);
    }
}
