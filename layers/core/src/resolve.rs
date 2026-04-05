//! Name-to-ID resolution utilities.
//!
//! When a CLI user types a resource name (e.g., `my-vpc`), these helpers
//! determine whether the input is already an ID (starts with the resource
//! prefix) or needs to be resolved via the daemon.

use serde::{Deserialize, Serialize};

/// Supported resource types for name resolution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceType {
    Org,
    Project,
    Env,
    Vpc,
    Subnet,
    Sg,
    Hypervisor,
    Vm,
    Volume,
    Snapshot,
    NatGw,
    RouteTable,
}

impl ResourceType {
    /// Return the ID prefix for this resource type.
    pub fn prefix(&self) -> &'static str {
        match self {
            ResourceType::Org => "org",
            ResourceType::Project => "proj",
            ResourceType::Env => "env",
            ResourceType::Vpc => "vpc",
            ResourceType::Subnet => "sub",
            ResourceType::Sg => "sg",
            ResourceType::Hypervisor => "hv",
            ResourceType::Vm => "vm",
            ResourceType::Volume => "vol",
            ResourceType::Snapshot => "snap",
            ResourceType::NatGw => "nat",
            ResourceType::RouteTable => "rt",
        }
    }

    /// Parse a resource type from a string label.
    pub fn from_str_label(s: &str) -> Option<Self> {
        match s {
            "org" => Some(ResourceType::Org),
            "project" => Some(ResourceType::Project),
            "env" => Some(ResourceType::Env),
            "vpc" => Some(ResourceType::Vpc),
            "subnet" => Some(ResourceType::Subnet),
            "sg" => Some(ResourceType::Sg),
            "hypervisor" => Some(ResourceType::Hypervisor),
            "vm" => Some(ResourceType::Vm),
            "volume" => Some(ResourceType::Volume),
            "snapshot" => Some(ResourceType::Snapshot),
            "nat-gw" => Some(ResourceType::NatGw),
            "route-table" => Some(ResourceType::RouteTable),
            _ => None,
        }
    }

    /// Return the label for this resource type.
    pub fn label(&self) -> &'static str {
        match self {
            ResourceType::Org => "org",
            ResourceType::Project => "project",
            ResourceType::Env => "env",
            ResourceType::Vpc => "vpc",
            ResourceType::Subnet => "subnet",
            ResourceType::Sg => "sg",
            ResourceType::Hypervisor => "hypervisor",
            ResourceType::Vm => "vm",
            ResourceType::Volume => "volume",
            ResourceType::Snapshot => "snapshot",
            ResourceType::NatGw => "nat-gw",
            ResourceType::RouteTable => "route-table",
        }
    }
}

/// Check whether the input looks like an existing ID (starts with the
/// resource prefix followed by a hyphen).
///
/// ```
/// use syfrah_core::resolve::{looks_like_id, ResourceType};
/// assert!(looks_like_id("vpc-a1b2c3d4e5f6", &ResourceType::Vpc));
/// assert!(!looks_like_id("my-vpc", &ResourceType::Vpc));
/// assert!(looks_like_id("vm-deadbeef1234", &ResourceType::Vm));
/// assert!(!looks_like_id("web-1", &ResourceType::Vm));
/// ```
pub fn looks_like_id(input: &str, resource_type: &ResourceType) -> bool {
    let prefix = resource_type.prefix();
    input.starts_with(&format!("{prefix}-"))
}

/// A resolved resource: either the input was already an ID, or it was
/// resolved to one (or multiple) via the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolveResult {
    /// Single ID resolved (or passed through).
    Id(String),
    /// Ambiguous: multiple resources matched the name.
    Ambiguous(Vec<ResolveMatch>),
    /// No resource found with this name.
    NotFound { resource_type: String, name: String },
}

/// A single match in an ambiguous resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveMatch {
    /// The resource ID.
    pub id: String,
    /// Disambiguation context (e.g., org name, VPC name).
    pub context: String,
}

impl std::fmt::Display for ResolveResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveResult::Id(id) => write!(f, "{id}"),
            ResolveResult::Ambiguous(matches) => {
                writeln!(f, "multiple resources matched:")?;
                for m in matches {
                    writeln!(f, "  {} ({})", m.id, m.context)?;
                }
                Ok(())
            }
            ResolveResult::NotFound {
                resource_type,
                name,
            } => write!(f, "{resource_type} '{name}' not found"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_like_id_detects_prefix() {
        assert!(looks_like_id("vpc-a1b2c3d4e5f6", &ResourceType::Vpc));
        assert!(looks_like_id("org-abc123def456", &ResourceType::Org));
        assert!(looks_like_id("vm-000000000000", &ResourceType::Vm));
        assert!(looks_like_id("hv-deadbeef1234", &ResourceType::Hypervisor));
        assert!(looks_like_id("vol-aabbccddee11", &ResourceType::Volume));
        assert!(looks_like_id("sub-123456789abc", &ResourceType::Subnet));
        assert!(looks_like_id("sg-aabbccddee11", &ResourceType::Sg));
        assert!(looks_like_id("snap-aabbccddee11", &ResourceType::Snapshot));
    }

    #[test]
    fn looks_like_id_rejects_names() {
        assert!(!looks_like_id("my-vpc", &ResourceType::Vpc));
        assert!(!looks_like_id("web-1", &ResourceType::Vm));
        assert!(!looks_like_id("acme", &ResourceType::Org));
        assert!(!looks_like_id("frontend", &ResourceType::Subnet));
        assert!(!looks_like_id("prod-db", &ResourceType::Volume));
    }

    #[test]
    fn looks_like_id_prefix_only_is_not_id() {
        // Just the prefix with no suffix is not an ID
        assert!(!looks_like_id("vpc", &ResourceType::Vpc));
        assert!(!looks_like_id("vm", &ResourceType::Vm));
    }

    #[test]
    fn resource_type_roundtrip() {
        for rt in &[
            ResourceType::Org,
            ResourceType::Project,
            ResourceType::Env,
            ResourceType::Vpc,
            ResourceType::Subnet,
            ResourceType::Sg,
            ResourceType::Hypervisor,
            ResourceType::Vm,
            ResourceType::Volume,
            ResourceType::Snapshot,
            ResourceType::NatGw,
            ResourceType::RouteTable,
        ] {
            let label = rt.label();
            let parsed = ResourceType::from_str_label(label).unwrap();
            assert_eq!(&parsed, rt);
        }
    }

    #[test]
    fn resolve_result_display() {
        let id = ResolveResult::Id("vpc-a1b2c3d4e5f6".into());
        assert_eq!(id.to_string(), "vpc-a1b2c3d4e5f6");

        let not_found = ResolveResult::NotFound {
            resource_type: "vpc".into(),
            name: "ghost".into(),
        };
        assert_eq!(not_found.to_string(), "vpc 'ghost' not found");

        let ambiguous = ResolveResult::Ambiguous(vec![
            ResolveMatch {
                id: "vpc-aaa".into(),
                context: "org: acme".into(),
            },
            ResolveMatch {
                id: "vpc-bbb".into(),
                context: "org: other".into(),
            },
        ]);
        let display = ambiguous.to_string();
        assert!(display.contains("multiple resources matched"));
        assert!(display.contains("vpc-aaa"));
        assert!(display.contains("vpc-bbb"));
    }

    #[test]
    fn serde_roundtrip() {
        let result = ResolveResult::Ambiguous(vec![ResolveMatch {
            id: "vpc-abc".into(),
            context: "org: test".into(),
        }]);
        let json = serde_json::to_string(&result).unwrap();
        let back: ResolveResult = serde_json::from_str(&json).unwrap();
        match back {
            ResolveResult::Ambiguous(matches) => {
                assert_eq!(matches.len(), 1);
                assert_eq!(matches[0].id, "vpc-abc");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}
