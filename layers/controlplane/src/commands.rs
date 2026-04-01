//! State machine command and response types for Raft-replicated operations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A command that is replicated through Raft and applied to the state machine.
///
/// Each variant maps to an existing store method in the org/hypervisor/ipam layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StateMachineCommand {
    // -- Org --
    CreateOrg {
        name: String,
    },
    DeleteOrg {
        name: String,
    },

    // -- Project --
    CreateProject {
        name: String,
        org: String,
    },
    DeleteProject {
        name: String,
        org: String,
    },

    // -- Environment --
    CreateEnv {
        name: String,
        project: String,
        org: String,
        ttl: Option<u64>,
        deletion_protection: bool,
        labels: HashMap<String, String>,
    },
    DeleteEnv {
        name: String,
        project: String,
        org: String,
    },

    // -- VPC --
    CreateVpc {
        name: String,
        cidr: String,
        owner: String,
        shared: bool,
    },
    DeleteVpc {
        name: String,
    },
    PeerVpc {
        vpc_a: String,
        vpc_b: String,
    },
    UnpeerVpc {
        vpc_a: String,
        vpc_b: String,
    },

    // -- Subnet --
    CreateSubnet {
        name: String,
        vpc: String,
        env_id: String,
        cidr: Option<String>,
    },
    DeleteSubnet {
        name: String,
        vpc: String,
    },

    // -- IPAM --
    AllocateIp {
        subnet_id: String,
    },
    ReleaseIp {
        subnet_id: String,
        ip: String,
    },

    // -- Security Groups --
    CreateSg {
        name: String,
        vpc: String,
    },
    DeleteSg {
        name: String,
    },
    AddSgRule {
        sg: String,
        direction: String,
        protocol: String,
        port: Option<String>,
        source: String,
    },
    RemoveSgRule {
        sg: String,
        rule_id: String,
    },
    AttachSg {
        sg: String,
        nic_id: String,
    },
    DetachSg {
        sg: String,
        nic_id: String,
    },

    // -- NAT Gateway --
    CreateNatGw {
        name: String,
        vpc: String,
        subnet: String,
    },
    DeleteNatGw {
        name: String,
    },

    // -- Routes --
    AddRoute {
        vpc: String,
        destination: String,
        target: String,
    },
    DeleteRoute {
        vpc: String,
        destination: String,
    },

    // -- VM Placement --
    PlaceVm {
        vm_id: String,
        hypervisor_id: String,
        subnet_id: String,
        ip: String,
        mac: String,
        generation: u64,
    },
    RemoveVm {
        vm_id: String,
    },
    RescheduleVm {
        vm_id: String,
        from: String,
        to: String,
        generation: u64,
    },

    // -- Hypervisor --
    RegisterHypervisor {
        name: String,
        region: String,
        zone: String,
    },
    EnableHypervisor {
        name: String,
    },
    DrainHypervisor {
        name: String,
    },
    DecommissionHypervisor {
        name: String,
    },
    UpdateHypervisorLabels {
        name: String,
        labels: HashMap<String, String>,
    },
    UpdateHypervisorTaints {
        name: String,
        taints: Vec<String>,
    },

    // -- NIC --
    CreateNic {
        vm_id: String,
        subnet_id: String,
        ip: String,
        mac: String,
    },
    DeleteNic {
        nic_id: String,
    },
}

impl std::fmt::Display for StateMachineCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateOrg { name } => write!(f, "CreateOrg({name})"),
            Self::DeleteOrg { name } => write!(f, "DeleteOrg({name})"),
            Self::CreateProject { name, org } => write!(f, "CreateProject({name}@{org})"),
            Self::DeleteProject { name, org } => write!(f, "DeleteProject({name}@{org})"),
            Self::CreateVpc { name, .. } => write!(f, "CreateVpc({name})"),
            Self::DeleteVpc { name } => write!(f, "DeleteVpc({name})"),
            Self::RegisterHypervisor { name, .. } => write!(f, "RegisterHypervisor({name})"),
            Self::EnableHypervisor { name } => write!(f, "EnableHypervisor({name})"),
            _ => write!(f, "{:?}", std::mem::discriminant(self)),
        }
    }
}

/// Response from the state machine after applying a command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum StateMachineResponse {
    /// Operation succeeded with no additional data.
    #[default]
    Ok,
    /// A resource was created; contains its ID.
    Created(String),
    /// Operation failed with an error message.
    Error(String),
    /// IP allocation result.
    AllocatedIp { ip: String, mac: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_serde_roundtrip() {
        let cmd = StateMachineCommand::CreateOrg {
            name: "acme".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let deserialized: StateMachineCommand = serde_json::from_str(&json).unwrap();
        match deserialized {
            StateMachineCommand::CreateOrg { name } => assert_eq!(name, "acme"),
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn response_serde_roundtrip() {
        let resp = StateMachineResponse::AllocatedIp {
            ip: "10.0.0.1".to_string(),
            mac: "02:00:0a:00:00:01".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deserialized: StateMachineResponse = serde_json::from_str(&json).unwrap();
        match deserialized {
            StateMachineResponse::AllocatedIp { ip, mac } => {
                assert_eq!(ip, "10.0.0.1");
                assert_eq!(mac, "02:00:0a:00:00:01");
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn command_display() {
        let cmd = StateMachineCommand::CreateOrg {
            name: "acme".to_string(),
        };
        assert_eq!(format!("{cmd}"), "CreateOrg(acme)");
    }
}
