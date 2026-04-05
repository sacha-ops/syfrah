//! Typed resource IDs backed by ULID.
//!
//! Every resource has a generated, immutable ID with a known prefix.
//! IDs are the primary key everywhere — Raft, stores, API, logs.
//! Names are for humans, IDs are for machines.
//!
//! Format: `{prefix}-{26-char-crockford-base32}` (e.g., `vpc-01J5A3K7G8MN2P4Q6R9S0T1V2W`)
//!
//! Properties:
//! - **Sortable**: IDs sort chronologically (ULID encodes timestamp)
//! - **Unique**: 80 bits of randomness per millisecond
//! - **Parseable**: prefix tells you the resource type
//! - **Validated**: `FromStr` / `TryFrom` reject malformed IDs
//!
//! # Usage
//!
//! ```
//! use syfrah_core::id::VpcId;
//!
//! let id = VpcId::generate();
//! assert!(id.as_str().starts_with("vpc-"));
//!
//! // Parse with validation
//! let parsed: VpcId = "vpc-01J5A3K7G8MN2P4Q6R9S0T1V2W".parse().unwrap();
//!
//! // Invalid prefix is rejected
//! assert!("org-01J5A3K7G8MN2P4Q6R9S0T1V2W".parse::<VpcId>().is_err());
//! ```

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Error returned when an ID string is invalid.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IdError {
    #[error("invalid {kind} ID: expected prefix '{expected_prefix}-', got '{input}'")]
    WrongPrefix {
        kind: &'static str,
        expected_prefix: &'static str,
        input: String,
    },
    #[error("invalid {kind} ID: missing ULID after prefix in '{input}'")]
    MissingUlid { kind: &'static str, input: String },
    #[error("invalid {kind} ID: bad ULID encoding in '{input}': {reason}")]
    InvalidUlid {
        kind: &'static str,
        input: String,
        reason: String,
    },
}

/// Generate a ULID string (26-char Crockford base32, uppercase).
fn generate_ulid() -> String {
    ulid::Ulid::new().to_string()
}

/// Extract the timestamp (milliseconds since epoch) from a ULID string.
pub fn timestamp_from_ulid(ulid_str: &str) -> Option<u64> {
    ulid::Ulid::from_string(ulid_str)
        .ok()
        .map(|u| u.timestamp_ms())
}

/// Macro to define a typed ID newtype backed by ULID.
macro_rules! define_id {
    ($name:ident, $prefix:expr, $doc:expr) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, Default,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Generate a new ID with embedded timestamp + 80 bits of randomness.
            pub fn generate() -> Self {
                Self(format!(concat!($prefix, "-{}"), generate_ulid()))
            }

            /// Wrap an existing string. No validation — use `parse()` or `try_from()` for validation.
            pub fn from_string(s: impl Into<String>) -> Self {
                Self(s.into())
            }

            /// Borrow the inner string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// The prefix for this ID type.
            pub fn prefix() -> &'static str {
                $prefix
            }

            /// Extract the ULID portion (after the prefix and dash).
            pub fn ulid_part(&self) -> Option<&str> {
                self.0.strip_prefix(concat!($prefix, "-"))
            }

            /// Extract the creation timestamp (ms since epoch) from the embedded ULID.
            pub fn created_at_ms(&self) -> Option<u64> {
                self.ulid_part().and_then(timestamp_from_ulid)
            }

            /// Check if a string is a valid ID of this type.
            pub fn is_valid(s: &str) -> bool {
                Self::validate(s).is_ok()
            }

            /// Check if the input looks like an ID of this type (starts with prefix-).
            pub fn looks_like_id(s: &str) -> bool {
                s.starts_with(concat!($prefix, "-"))
            }

            /// Full validation.
            fn validate(s: &str) -> Result<(), IdError> {
                let prefix_dash = concat!($prefix, "-");
                if !s.starts_with(prefix_dash) {
                    return Err(IdError::WrongPrefix {
                        kind: $prefix,
                        expected_prefix: $prefix,
                        input: s.to_string(),
                    });
                }
                let ulid_part = &s[prefix_dash.len()..];
                if ulid_part.is_empty() {
                    return Err(IdError::MissingUlid {
                        kind: $prefix,
                        input: s.to_string(),
                    });
                }
                // Validate ULID encoding
                ulid::Ulid::from_string(ulid_part).map_err(|e| IdError::InvalidUlid {
                    kind: $prefix,
                    input: s.to_string(),
                    reason: e.to_string(),
                })?;
                Ok(())
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

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::validate(s)?;
                Ok(Self(s.to_string()))
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
        // "vpc-" (4) + 26 ULID chars = 30
        assert_eq!(id.as_str().len(), 30, "got: {id}");
    }

    #[test]
    fn generate_unique() {
        let a = VpcId::generate();
        let b = VpcId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn generate_sortable_chronologically() {
        let a = VpcId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = VpcId::generate();
        // ULID encodes timestamp first → lexicographic order = chronological order
        assert!(a.as_str() < b.as_str(), "expected {a} < {b}");
    }

    #[test]
    fn created_at_ms_extracts_timestamp() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let id = VpcId::generate();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;

        let ts = id.created_at_ms().unwrap();
        assert!(
            ts >= before && ts <= after,
            "ts={ts} not in [{before}, {after}]"
        );
    }

    #[test]
    fn from_str_valid() {
        let id = VpcId::generate();
        let parsed: VpcId = id.as_str().parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn from_str_wrong_prefix() {
        let result = "org-01J5A3K7G8MN2P4Q6R9S0T1V2W".parse::<VpcId>();
        assert!(result.is_err());
        match result.unwrap_err() {
            IdError::WrongPrefix {
                expected_prefix, ..
            } => {
                assert_eq!(expected_prefix, "vpc");
            }
            other => panic!("expected WrongPrefix, got: {other}"),
        }
    }

    #[test]
    fn from_str_missing_ulid() {
        let result = "vpc-".parse::<VpcId>();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IdError::MissingUlid { .. }));
    }

    #[test]
    fn from_str_invalid_ulid() {
        let result = "vpc-not_a_valid_ulid!!!!!!!".parse::<VpcId>();
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IdError::InvalidUlid { .. }));
    }

    #[test]
    fn from_str_no_prefix() {
        let result = "my-vpc".parse::<VpcId>();
        assert!(result.is_err());
    }

    #[test]
    fn parse_validated_vs_from_unchecked() {
        // parse() validates
        assert!("garbage".parse::<VpcId>().is_err());
        // From<&str> does not validate (for deserialization compat)
        let id: VpcId = "garbage".into();
        assert_eq!(id.as_str(), "garbage");
    }

    #[test]
    fn is_valid() {
        let id = VpcId::generate();
        assert!(VpcId::is_valid(id.as_str()));
        assert!(!VpcId::is_valid("org-01J5A3K7G8MN2P4Q6R9S0T1V2W"));
        assert!(!VpcId::is_valid("vpc-"));
        assert!(!VpcId::is_valid("vpc"));
        assert!(!VpcId::is_valid("my-vpc"));
    }

    #[test]
    fn looks_like_id() {
        assert!(VpcId::looks_like_id("vpc-anything"));
        assert!(!VpcId::looks_like_id("my-vpc"));
        assert!(!VpcId::looks_like_id("vpc"));
    }

    #[test]
    fn serde_roundtrip() {
        let id = VpcId::generate();
        let json = serde_json::to_string(&id).unwrap();
        let back: VpcId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
        // Serializes as plain string, not object
        assert!(json.starts_with('"'));
    }

    #[test]
    fn display() {
        let id = VpcId::generate();
        let s = format!("{id}");
        assert!(s.starts_with("vpc-"));
    }

    #[test]
    fn eq_with_str() {
        let id = VpcId::from_string("vpc-test");
        assert!(id == "vpc-test");
        assert!(id == *"vpc-test");
        let s = "vpc-test".to_string();
        assert!(id == s);
    }

    #[test]
    fn deref_to_str() {
        let id = VpcId::generate();
        assert!(id.starts_with("vpc-"));
        assert!(id.len() == 30);
    }

    #[test]
    fn prefix() {
        assert_eq!(VpcId::prefix(), "vpc");
        assert_eq!(OrgId::prefix(), "org");
        assert_eq!(HypervisorId::prefix(), "hv");
        assert_eq!(NodeId::prefix(), "node");
        assert_eq!(MeshId::prefix(), "mesh");
    }

    #[test]
    fn ulid_part_extracts_correctly() {
        let id = VpcId::generate();
        let ulid = id.ulid_part().unwrap();
        assert_eq!(ulid.len(), 26);
        // Verify it's valid ULID
        assert!(ulid::Ulid::from_string(ulid).is_ok());
    }

    #[test]
    fn all_types_generate() {
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
        let id = VpcId::generate();
        let key = id.as_str().to_string();
        map.insert(id, "test");
        assert!(map.contains_key(key.as_str()));
    }

    #[test]
    fn ord_is_chronological() {
        let mut ids: Vec<VpcId> = (0..5)
            .map(|_| {
                std::thread::sleep(std::time::Duration::from_millis(2));
                VpcId::generate()
            })
            .collect();
        let sorted = ids.clone();
        ids.sort();
        assert_eq!(
            ids, sorted,
            "ULID-based IDs should already be in chronological order"
        );
    }
}
