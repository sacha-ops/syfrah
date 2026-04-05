//! Typed resource IDs.
//!
//! Every resource has a generated, immutable ID with a known prefix.
//! IDs are the primary key everywhere — Raft, stores, API, logs.
//! Names are for humans, IDs are for machines.
//!
//! Format: `{prefix}-{12-hex-chars}` (e.g., `vpc-a1b2c3d4e5f6`)
//!
//! # Usage
//!
//! ```
//! use syfrah_core::id::VpcId;
//!
//! let id = VpcId::generate();
//! assert!(id.as_str().starts_with("vpc-"));
//! assert_eq!(id.as_str().len(), 16); // "vpc-" + 12 hex
//! ```

use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Generate a random ID with the given prefix.
fn generate_id(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let hex: String = (0..12)
        .map(|_| format!("{:x}", rng.gen_range(0..16)))
        .collect();
    format!("{prefix}-{hex}")
}

/// Generate a deterministic ID from a prefix + seed (for migrations).
pub fn deterministic_id(prefix: &str, seed: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    prefix.hash(&mut hasher);
    seed.hash(&mut hasher);
    let hash = hasher.finish();
    format!("{prefix}-{hash:012x}")
}

/// Define a typed ID newtype.
///
/// Each ID type gets:
/// - `generate()` — create a new random ID
/// - `from_string()` / `From<String>` / `From<&str>` — wrap an existing string
/// - `as_str()` — borrow as `&str`
/// - `prefix()` — the static prefix (e.g., "vpc")
/// - `is_valid()` — check if a string looks like this ID type
/// - `Serialize` / `Deserialize` as transparent strings
/// - `Display`, `Hash`, `Eq`, `Ord`, `Clone`, `Debug`
macro_rules! define_id {
    ($name:ident, $prefix:expr, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, Default,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a new random ID.
            pub fn generate() -> Self {
                Self(generate_id($prefix))
            }

            /// Wrap an existing string as this ID type.
            pub fn from_string(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The prefix for this ID type (e.g., "vpc").
            pub fn prefix() -> &'static str {
                $prefix
            }

            /// Check if a string looks like a valid ID of this type.
            pub fn is_valid(s: &str) -> bool {
                s.starts_with(concat!($prefix, "-")) && s.len() > concat!($prefix, "-").len()
            }

            /// Check if the given input looks like an ID (vs a human name).
            /// Used for name-or-id resolution.
            pub fn looks_like_id(s: &str) -> bool {
                s.starts_with(concat!($prefix, "-"))
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

        impl std::ops::Deref for $name {
            type Target = str;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::borrow::Borrow<str> for $name {
            fn borrow(&self) -> &str {
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

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                self.0 == *other
            }
        }
    };
}

define_id!(OrgId, "org", "Organization ID");
define_id!(ProjectId, "proj", "Project ID");
define_id!(EnvId, "env", "Environment ID");
define_id!(VpcId, "vpc", "VPC ID");
define_id!(SubnetId, "sub", "Subnet ID");
define_id!(SgId, "sg", "Security Group ID");
define_id!(HypervisorId, "hv", "Hypervisor ID");
define_id!(VmId, "vm", "Virtual Machine ID");
define_id!(VolumeId, "vol", "Volume ID");
define_id!(SnapshotId, "snap", "Snapshot ID");
define_id!(NicId, "nic", "Network Interface ID");
define_id!(NatGwId, "nat", "NAT Gateway ID");
define_id!(RouteTableId, "rt", "Route Table ID");
define_id!(RuleId, "rule", "Security Group Rule ID");
define_id!(PeeringId, "peer", "VPC Peering ID");
define_id!(NodeId, "node", "Fabric Node ID");
define_id!(MeshId, "mesh", "Mesh Network ID");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_has_correct_prefix() {
        let id = VpcId::generate();
        assert!(id.as_str().starts_with("vpc-"), "got: {id}");
    }

    #[test]
    fn generate_correct_length() {
        let id = VpcId::generate();
        // "vpc-" (4) + 12 hex chars = 16
        assert_eq!(id.as_str().len(), 16, "got: {id}");
    }

    #[test]
    fn generate_unique() {
        let a = VpcId::generate();
        let b = VpcId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn from_string() {
        let id = VpcId::from_string("vpc-custom123456");
        assert_eq!(id.as_str(), "vpc-custom123456");
    }

    #[test]
    fn from_str() {
        let id: VpcId = "vpc-abc".into();
        assert_eq!(id.as_str(), "vpc-abc");
    }

    #[test]
    fn display() {
        let id = VpcId::from_string("vpc-abc");
        assert_eq!(format!("{id}"), "vpc-abc");
    }

    #[test]
    fn is_valid() {
        assert!(VpcId::is_valid("vpc-abc123def456"));
        assert!(!VpcId::is_valid("org-abc123"));
        assert!(!VpcId::is_valid("vpc-"));
        assert!(!VpcId::is_valid("vpc"));
        assert!(!VpcId::is_valid("my-vpc"));
    }

    #[test]
    fn looks_like_id() {
        assert!(VpcId::looks_like_id("vpc-abc123"));
        assert!(!VpcId::looks_like_id("my-vpc"));
        assert!(!VpcId::looks_like_id("vpc"));
    }

    #[test]
    fn serde_roundtrip() {
        let id = VpcId::from_string("vpc-abc123def456");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""vpc-abc123def456""#);
        let back: VpcId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn eq_with_str() {
        let id = VpcId::from_string("vpc-abc");
        assert!(id == "vpc-abc");
        assert!(id == *"vpc-abc");
        let s = "vpc-abc".to_string();
        assert!(id == s);
    }

    #[test]
    fn deref_to_str() {
        let id = VpcId::from_string("vpc-abc");
        assert!(id.starts_with("vpc-")); // uses Deref<Target=str>
        assert_eq!(id.len(), 7);
    }

    #[test]
    fn prefix() {
        assert_eq!(VpcId::prefix(), "vpc");
        assert_eq!(OrgId::prefix(), "org");
        assert_eq!(HypervisorId::prefix(), "hv");
        assert_eq!(NodeId::prefix(), "node");
    }

    #[test]
    fn deterministic_id_is_stable() {
        let a = deterministic_id("vpc", "my-vpc");
        let b = deterministic_id("vpc", "my-vpc");
        assert_eq!(a, b);
        assert!(a.starts_with("vpc-"));
    }

    #[test]
    fn deterministic_id_differs_by_seed() {
        let a = deterministic_id("vpc", "vpc-a");
        let b = deterministic_id("vpc", "vpc-b");
        assert_ne!(a, b);
    }

    #[test]
    fn all_types_generate() {
        // Verify every ID type compiles and generates
        let _ = OrgId::generate();
        let _ = ProjectId::generate();
        let _ = EnvId::generate();
        let _ = VpcId::generate();
        let _ = SubnetId::generate();
        let _ = SgId::generate();
        let _ = HypervisorId::generate();
        let _ = VmId::generate();
        let _ = VolumeId::generate();
        let _ = SnapshotId::generate();
        let _ = NicId::generate();
        let _ = NatGwId::generate();
        let _ = RouteTableId::generate();
        let _ = RuleId::generate();
        let _ = PeeringId::generate();
        let _ = NodeId::generate();
        let _ = MeshId::generate();
    }

    #[test]
    fn default_is_empty() {
        let id = VpcId::default();
        assert_eq!(id.as_str(), "");
    }

    #[test]
    fn hash_works_in_hashmap() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let id = VpcId::generate();
        map.insert(id.clone(), "my-vpc");
        assert_eq!(map.get(&id), Some(&"my-vpc"));
    }

    #[test]
    fn borrow_str_for_hashmap_lookup() {
        use std::collections::HashMap;
        let mut map: HashMap<VpcId, &str> = HashMap::new();
        let id = VpcId::from_string("vpc-abc");
        map.insert(id, "test");
        // Can lookup with &str thanks to Borrow<str>
        assert!(map.contains_key("vpc-abc"));
    }
}
