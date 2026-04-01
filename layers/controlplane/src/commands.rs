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

    #[test]
    fn all_command_variants_serialize() {
        // Ensure every variant can be serialized and deserialized.
        let commands = vec![
            StateMachineCommand::CreateOrg { name: "a".into() },
            StateMachineCommand::DeleteOrg { name: "a".into() },
            StateMachineCommand::CreateProject {
                name: "b".into(),
                org: "a".into(),
            },
            StateMachineCommand::DeleteProject {
                name: "b".into(),
                org: "a".into(),
            },
            StateMachineCommand::CreateEnv {
                name: "dev".into(),
                project: "b".into(),
                org: "a".into(),
                ttl: Some(3600),
                deletion_protection: true,
                labels: HashMap::from([("env".into(), "dev".into())]),
            },
            StateMachineCommand::DeleteEnv {
                name: "dev".into(),
                project: "b".into(),
                org: "a".into(),
            },
            StateMachineCommand::CreateVpc {
                name: "v".into(),
                cidr: "10.0.0.0/16".into(),
                owner: "a".into(),
                shared: false,
            },
            StateMachineCommand::DeleteVpc { name: "v".into() },
            StateMachineCommand::PeerVpc {
                vpc_a: "v1".into(),
                vpc_b: "v2".into(),
            },
            StateMachineCommand::UnpeerVpc {
                vpc_a: "v1".into(),
                vpc_b: "v2".into(),
            },
            StateMachineCommand::CreateSubnet {
                name: "s".into(),
                vpc: "v".into(),
                env_id: "e".into(),
                cidr: Some("10.0.1.0/24".into()),
            },
            StateMachineCommand::DeleteSubnet {
                name: "s".into(),
                vpc: "v".into(),
            },
            StateMachineCommand::AllocateIp {
                subnet_id: "s1".into(),
            },
            StateMachineCommand::ReleaseIp {
                subnet_id: "s1".into(),
                ip: "10.0.0.1".into(),
            },
            StateMachineCommand::CreateSg {
                name: "sg".into(),
                vpc: "v".into(),
            },
            StateMachineCommand::DeleteSg { name: "sg".into() },
            StateMachineCommand::AddSgRule {
                sg: "sg".into(),
                direction: "ingress".into(),
                protocol: "tcp".into(),
                port: Some("443".into()),
                source: "0.0.0.0/0".into(),
            },
            StateMachineCommand::RemoveSgRule {
                sg: "sg".into(),
                rule_id: "r1".into(),
            },
            StateMachineCommand::AttachSg {
                sg: "sg".into(),
                nic_id: "nic1".into(),
            },
            StateMachineCommand::DetachSg {
                sg: "sg".into(),
                nic_id: "nic1".into(),
            },
            StateMachineCommand::CreateNatGw {
                name: "nat".into(),
                vpc: "v".into(),
                subnet: "s".into(),
            },
            StateMachineCommand::DeleteNatGw { name: "nat".into() },
            StateMachineCommand::AddRoute {
                vpc: "v".into(),
                destination: "0.0.0.0/0".into(),
                target: "nat".into(),
            },
            StateMachineCommand::DeleteRoute {
                vpc: "v".into(),
                destination: "0.0.0.0/0".into(),
            },
            StateMachineCommand::PlaceVm {
                vm_id: "vm1".into(),
                hypervisor_id: "hv1".into(),
                subnet_id: "s1".into(),
                ip: "10.0.0.2".into(),
                mac: "02:00:00:00:00:01".into(),
                generation: 1,
            },
            StateMachineCommand::RemoveVm {
                vm_id: "vm1".into(),
            },
            StateMachineCommand::RescheduleVm {
                vm_id: "vm1".into(),
                from: "hv1".into(),
                to: "hv2".into(),
                generation: 2,
            },
            StateMachineCommand::RegisterHypervisor {
                name: "hv1".into(),
                region: "eu".into(),
                zone: "az1".into(),
            },
            StateMachineCommand::EnableHypervisor { name: "hv1".into() },
            StateMachineCommand::DrainHypervisor { name: "hv1".into() },
            StateMachineCommand::DecommissionHypervisor { name: "hv1".into() },
            StateMachineCommand::UpdateHypervisorLabels {
                name: "hv1".into(),
                labels: HashMap::new(),
            },
            StateMachineCommand::UpdateHypervisorTaints {
                name: "hv1".into(),
                taints: vec!["gpu=true:NoSchedule".into()],
            },
            StateMachineCommand::CreateNic {
                vm_id: "vm1".into(),
                subnet_id: "s1".into(),
                ip: "10.0.0.2".into(),
                mac: "02:00:00:00:00:01".into(),
            },
            StateMachineCommand::DeleteNic {
                nic_id: "nic1".into(),
            },
        ];

        for cmd in &commands {
            let json = serde_json::to_string(cmd).expect("serialize failed");
            let _: StateMachineCommand = serde_json::from_str(&json).expect("deserialize failed");
        }
    }

    #[test]
    fn all_response_variants_serialize() {
        let responses = vec![
            StateMachineResponse::Ok,
            StateMachineResponse::Created("id-1".into()),
            StateMachineResponse::Error("something failed".into()),
            StateMachineResponse::AllocatedIp {
                ip: "10.0.0.1".into(),
                mac: "02:00:00:00:00:01".into(),
            },
        ];
        for resp in &responses {
            let json = serde_json::to_string(resp).expect("serialize failed");
            let _: StateMachineResponse = serde_json::from_str(&json).expect("deserialize failed");
        }
    }

    #[test]
    fn default_response_is_ok() {
        assert!(matches!(
            StateMachineResponse::default(),
            StateMachineResponse::Ok
        ));
    }
}
