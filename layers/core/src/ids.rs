use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// ID generation
// ---------------------------------------------------------------------------

/// Generate a random ID with the given prefix.
/// Format: `{prefix}-{12-hex-chars}` (e.g., `org-a1b2c3d4e5f6`)
fn generate_id(prefix: &str) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let hex: String = (0..12)
        .map(|_| format!("{:x}", rng.gen::<u8>() % 16))
        .collect();
    format!("{prefix}-{hex}")
}

// ---------------------------------------------------------------------------
// Macro to define an ID newtype with all needed traits
// ---------------------------------------------------------------------------

macro_rules! define_id {
    ($name:ident, $prefix:expr) => {
        #[derive(Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Create a new random ID with the correct prefix.
            pub fn generate() -> Self {
                Self(generate_id($prefix))
            }

            /// Wrap an existing string as this ID type.
            pub fn from_string(s: String) -> Self {
                Self(s)
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The prefix for this ID type (e.g. `"org"`, `"vm"`).
            pub fn prefix() -> &'static str {
                $prefix
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }
    };
}

// ---------------------------------------------------------------------------
// ID types for every resource
// ---------------------------------------------------------------------------

define_id!(OrgId, "org");
define_id!(ProjectId, "proj");
define_id!(EnvId, "env");
define_id!(VpcId, "vpc");
define_id!(SubnetId, "sub");
define_id!(SgId, "sg");
define_id!(HypervisorId, "hv");
define_id!(VmId, "vm");
define_id!(VolumeId, "vol");
define_id!(SnapshotId, "snap");
define_id!(NicId, "nic");
define_id!(NatGwId, "nat");
define_id!(RouteTableId, "rt");
define_id!(RuleId, "rule");
define_id!(PeeringId, "peer");

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_has_correct_prefix() {
        assert!(OrgId::generate().as_str().starts_with("org-"));
        assert!(ProjectId::generate().as_str().starts_with("proj-"));
        assert!(EnvId::generate().as_str().starts_with("env-"));
        assert!(VpcId::generate().as_str().starts_with("vpc-"));
        assert!(SubnetId::generate().as_str().starts_with("sub-"));
        assert!(SgId::generate().as_str().starts_with("sg-"));
        assert!(HypervisorId::generate().as_str().starts_with("hv-"));
        assert!(VmId::generate().as_str().starts_with("vm-"));
        assert!(VolumeId::generate().as_str().starts_with("vol-"));
        assert!(SnapshotId::generate().as_str().starts_with("snap-"));
        assert!(NicId::generate().as_str().starts_with("nic-"));
        assert!(NatGwId::generate().as_str().starts_with("nat-"));
        assert!(RouteTableId::generate().as_str().starts_with("rt-"));
        assert!(RuleId::generate().as_str().starts_with("rule-"));
        assert!(PeeringId::generate().as_str().starts_with("peer-"));
    }

    #[test]
    fn generate_produces_unique_ids() {
        let ids: HashSet<String> = (0..100).map(|_| VmId::generate().0).collect();
        assert_eq!(ids.len(), 100, "100 generated IDs should all be unique");
    }

    #[test]
    fn serde_roundtrip() {
        let id = VmId::from_string("vm-aabbccddee11".to_string());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(
            json, "\"vm-aabbccddee11\"",
            "should serialize as plain string"
        );
        let back: VmId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn display_and_from() {
        let id = OrgId::from("org-test123");
        assert_eq!(id.to_string(), "org-test123");
        assert_eq!(id.as_str(), "org-test123");
        assert_eq!(id.as_ref(), "org-test123");

        let id2: OrgId = "org-test456".into();
        assert_eq!(id2.to_string(), "org-test456");

        let id3: OrgId = String::from("org-test789").into();
        assert_eq!(id3.to_string(), "org-test789");
    }

    #[test]
    fn hash_and_eq() {
        let a = VpcId::from("vpc-aaa");
        let b = VpcId::from("vpc-aaa");
        let c = VpcId::from("vpc-bbb");

        assert_eq!(a, b);
        assert_ne!(a, c);

        let mut set = HashSet::new();
        set.insert(a.clone());
        set.insert(b.clone());
        assert_eq!(set.len(), 1, "equal IDs should hash the same");
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn ord() {
        let a = SubnetId::from("sub-aaa");
        let b = SubnetId::from("sub-zzz");
        assert!(a < b);
    }

    #[test]
    fn prefix_accessor() {
        assert_eq!(OrgId::prefix(), "org");
        assert_eq!(VmId::prefix(), "vm");
        assert_eq!(SnapshotId::prefix(), "snap");
    }
}
