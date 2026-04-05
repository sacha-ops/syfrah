//! Version management and update channels.
//!
//! Supports 4 channels: dev, beta, rc, stable.
//! Versions are semver with channel suffix: `v2.1.0-dev.47`, `v2.1.0`, etc.
//!
//! ```
//! use syfrah_core::version::{Version, Channel};
//!
//! let v = Version::parse("2.1.0-beta.3").unwrap();
//! assert_eq!(v.channel, Channel::Beta);
//! assert_eq!(v.major, 2);
//! assert!(v > Version::parse("2.1.0-beta.2").unwrap());
//! ```

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;

use crate::error::SyfrahError;

/// Release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Dev,
    Beta,
    Rc,
    Stable,
}

impl Channel {
    /// Stability rank (higher = more stable).
    pub fn rank(&self) -> u8 {
        match self {
            Self::Dev => 0,
            Self::Beta => 1,
            Self::Rc => 2,
            Self::Stable => 3,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Beta => "beta",
            Self::Rc => "rc",
            Self::Stable => "stable",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Channel {
    type Err = SyfrahError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "dev" => Ok(Self::Dev),
            "beta" => Ok(Self::Beta),
            "rc" => Ok(Self::Rc),
            "stable" | "latest" => Ok(Self::Stable),
            _ => Err(SyfrahError::validation(format!(
                "unknown channel '{s}'. Must be: dev, beta, rc, stable"
            ))),
        }
    }
}

/// A parsed version.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub channel: Channel,
    /// Build number within channel (e.g., 47 in dev.47). 0 for stable.
    pub build: u32,
}

impl Version {
    /// Parse a version string. Accepts with or without `v` prefix.
    pub fn parse(s: &str) -> Result<Self, SyfrahError> {
        let s = s.strip_prefix('v').unwrap_or(s);

        // Split semver from pre-release
        let (semver, pre) = if let Some(pos) = s.find('-') {
            (&s[..pos], Some(&s[pos + 1..]))
        } else {
            (s, None)
        };

        // Parse major.minor.patch
        let parts: Vec<&str> = semver.split('.').collect();
        if parts.len() != 3 {
            return Err(SyfrahError::validation(format!(
                "invalid version '{s}': expected MAJOR.MINOR.PATCH"
            )));
        }

        let major: u32 = parts[0]
            .parse()
            .map_err(|_| SyfrahError::validation(format!("invalid major version: {}", parts[0])))?;
        let minor: u32 = parts[1]
            .parse()
            .map_err(|_| SyfrahError::validation(format!("invalid minor version: {}", parts[1])))?;
        let patch: u32 = parts[2]
            .parse()
            .map_err(|_| SyfrahError::validation(format!("invalid patch version: {}", parts[2])))?;

        // Parse pre-release channel
        let (channel, build) = match pre {
            None => (Channel::Stable, 0),
            Some(pre) => {
                let pre_parts: Vec<&str> = pre.splitn(2, '.').collect();
                let ch: Channel = pre_parts[0].parse()?;
                let build: u32 = if pre_parts.len() > 1 {
                    pre_parts[1].parse().map_err(|_| {
                        SyfrahError::validation(format!("invalid build number: {}", pre_parts[1]))
                    })?
                } else {
                    0
                };
                (ch, build)
            }
        };

        Ok(Self {
            major,
            minor,
            patch,
            channel,
            build,
        })
    }

    /// Current binary version (injected at build time or from Cargo.toml).
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
            channel: Channel::Dev,
            build: 0,
        })
    }

    /// Is this a pre-release version?
    pub fn is_prerelease(&self) -> bool {
        self.channel != Channel::Stable
    }

    /// Check if this version is newer than another.
    pub fn is_newer_than(&self, other: &Self) -> bool {
        self > other
    }

    /// Format as GitHub tag: `v2.1.0-beta.3` or `v2.1.0`.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if self.channel != Channel::Stable {
            write!(f, "-{}.{}", self.channel, self.build)?;
        }
        Ok(())
    }
}

impl FromStr for Version {
    type Err = SyfrahError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
            .then(self.channel.rank().cmp(&other.channel.rank()))
            .then(self.build.cmp(&other.build))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Feature flag — runtime gating of experimental features.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeatureFlags {
    flags: std::collections::HashMap<String, bool>,
}

impl FeatureFlags {
    pub fn new() -> Self {
        Self {
            flags: std::collections::HashMap::new(),
        }
    }

    /// Check if a feature is enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }

    /// Enable a feature.
    pub fn enable(&mut self, name: impl Into<String>) {
        self.flags.insert(name.into(), true);
    }

    /// Disable a feature.
    pub fn disable(&mut self, name: impl Into<String>) {
        self.flags.insert(name.into(), false);
    }
}

impl Default for FeatureFlags {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Channel ──

    #[test]
    fn channel_parse() {
        assert_eq!("dev".parse::<Channel>().unwrap(), Channel::Dev);
        assert_eq!("beta".parse::<Channel>().unwrap(), Channel::Beta);
        assert_eq!("rc".parse::<Channel>().unwrap(), Channel::Rc);
        assert_eq!("stable".parse::<Channel>().unwrap(), Channel::Stable);
        assert_eq!("latest".parse::<Channel>().unwrap(), Channel::Stable);
    }

    #[test]
    fn channel_invalid() {
        assert!("nope".parse::<Channel>().is_err());
    }

    #[test]
    fn channel_rank() {
        assert!(Channel::Dev.rank() < Channel::Beta.rank());
        assert!(Channel::Beta.rank() < Channel::Rc.rank());
        assert!(Channel::Rc.rank() < Channel::Stable.rank());
    }

    // ── Version parsing ──

    #[test]
    fn parse_stable() {
        let v = Version::parse("2.1.0").unwrap();
        assert_eq!(v.major, 2);
        assert_eq!(v.minor, 1);
        assert_eq!(v.patch, 0);
        assert_eq!(v.channel, Channel::Stable);
        assert_eq!(v.build, 0);
    }

    #[test]
    fn parse_with_v_prefix() {
        let v = Version::parse("v2.1.0-beta.3").unwrap();
        assert_eq!(v.major, 2);
        assert_eq!(v.channel, Channel::Beta);
        assert_eq!(v.build, 3);
    }

    #[test]
    fn parse_dev() {
        let v = Version::parse("2.1.0-dev.47").unwrap();
        assert_eq!(v.channel, Channel::Dev);
        assert_eq!(v.build, 47);
    }

    #[test]
    fn parse_rc() {
        let v = Version::parse("2.1.0-rc.1").unwrap();
        assert_eq!(v.channel, Channel::Rc);
        assert_eq!(v.build, 1);
    }

    #[test]
    fn parse_invalid() {
        assert!(Version::parse("not-a-version").is_err());
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
    }

    // ── Display ──

    #[test]
    fn display_stable() {
        let v = Version::parse("2.1.0").unwrap();
        assert_eq!(v.to_string(), "2.1.0");
    }

    #[test]
    fn display_prerelease() {
        let v = Version::parse("2.1.0-beta.3").unwrap();
        assert_eq!(v.to_string(), "2.1.0-beta.3");
    }

    #[test]
    fn tag() {
        let v = Version::parse("2.1.0-beta.3").unwrap();
        assert_eq!(v.tag(), "v2.1.0-beta.3");
    }

    // ── Ordering ──

    #[test]
    fn ordering_semver() {
        let a = Version::parse("1.0.0").unwrap();
        let b = Version::parse("2.0.0").unwrap();
        assert!(b > a);
    }

    #[test]
    fn ordering_minor() {
        let a = Version::parse("2.0.0").unwrap();
        let b = Version::parse("2.1.0").unwrap();
        assert!(b > a);
    }

    #[test]
    fn ordering_patch() {
        let a = Version::parse("2.1.0").unwrap();
        let b = Version::parse("2.1.1").unwrap();
        assert!(b > a);
    }

    #[test]
    fn ordering_channel() {
        let dev = Version::parse("2.1.0-dev.1").unwrap();
        let beta = Version::parse("2.1.0-beta.1").unwrap();
        let rc = Version::parse("2.1.0-rc.1").unwrap();
        let stable = Version::parse("2.1.0").unwrap();
        assert!(dev < beta);
        assert!(beta < rc);
        assert!(rc < stable);
    }

    #[test]
    fn ordering_build() {
        let a = Version::parse("2.1.0-dev.1").unwrap();
        let b = Version::parse("2.1.0-dev.47").unwrap();
        assert!(b > a);
    }

    #[test]
    fn is_newer() {
        let old = Version::parse("2.0.0").unwrap();
        let new = Version::parse("2.1.0").unwrap();
        assert!(new.is_newer_than(&old));
        assert!(!old.is_newer_than(&new));
    }

    // ── Feature flags ──

    #[test]
    fn feature_flags_default_off() {
        let f = FeatureFlags::new();
        assert!(!f.is_enabled("async_vm"));
    }

    #[test]
    fn feature_flags_enable_disable() {
        let mut f = FeatureFlags::new();
        f.enable("async_vm");
        assert!(f.is_enabled("async_vm"));
        f.disable("async_vm");
        assert!(!f.is_enabled("async_vm"));
    }

    // ── Current version ──

    #[test]
    fn current_version_parses() {
        let v = Version::current();
        // Should parse without panic
        let _ = v.to_string();
    }

    #[test]
    fn is_prerelease() {
        assert!(Version::parse("2.0.0-dev.1").unwrap().is_prerelease());
        assert!(Version::parse("2.0.0-beta.1").unwrap().is_prerelease());
        assert!(!Version::parse("2.0.0").unwrap().is_prerelease());
    }

    // ── Serde ──

    #[test]
    fn version_serde() {
        let v = Version::parse("2.1.0-beta.3").unwrap();
        let json = serde_json::to_string(&v).unwrap();
        let back: Version = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn channel_serde() {
        let c = Channel::Beta;
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"beta\"");
    }
}
