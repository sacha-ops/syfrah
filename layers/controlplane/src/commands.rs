//! State machine command and response types for Raft-replicated operations.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Storage types (inline until syfrah-core #1178 lands proper typed IDs)
// ---------------------------------------------------------------------------

/// Volume type: root volumes are tied to a VM lifecycle, data volumes are independent.
// TODO: Replace with syfrah_core::VolumeType once #1178 lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum VolumeType {
    /// Root volume — auto-created with a VM, deleted when the VM is deleted.
    Root,
    /// Data volume — created independently, lifecycle is not tied to any VM.
    Data,
}

/// Per-region S3 storage configuration replicated through Raft.
///
/// # Security notes
/// - `s3_access_key` and `s3_secret_key` ARE stored in Raft so that all nodes
///   in the region know how to reach the S3 bucket.
/// - The `encryption_passphrase` is NOT stored in Raft. It is kept locally on
///   each hypervisor (file with 0600 permissions) and never replicated via
///   consensus. See ADR-006 §9.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StorageConfig {
    /// S3-compatible endpoint URL (e.g. "https://s3.par.io.cloud.ovh.net").
    pub s3_endpoint: String,
    /// S3 bucket name. All volumes in this region share this bucket.
    pub s3_bucket: String,
    /// S3 access key (replicated via Raft for cross-node access).
    pub s3_access_key: String,
    /// S3 secret key (replicated via Raft for cross-node access).
    pub s3_secret_key: String,
    /// Path to the local SSD used for the warm cache.
    pub cache_disk_path: String,
    /// Maximum SSD cache size in gigabytes.
    pub cache_disk_size_gb: u32,
    /// Maximum memory cache size in gigabytes.
    pub cache_memory_size_gb: u32,
}

// SECURITY: Custom Debug impl to prevent S3 secret key from appearing in logs
// or error messages. The access key is partially redacted and the secret key
// is fully redacted.
impl std::fmt::Debug for StorageConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageConfig")
            .field("s3_endpoint", &self.s3_endpoint)
            .field("s3_bucket", &self.s3_bucket)
            .field("s3_access_key", &"[REDACTED]")
            .field("s3_secret_key", &"[REDACTED]")
            .field("cache_disk_path", &self.cache_disk_path)
            .field("cache_disk_size_gb", &self.cache_disk_size_gb)
            .field("cache_memory_size_gb", &self.cache_memory_size_gb)
            .finish()
    }
}

impl StorageConfig {
    /// Validate the storage configuration.
    ///
    /// Returns `Ok(())` if the config is valid, or an error message describing
    /// what is wrong. Validation rules:
    /// - `s3_endpoint` must start with `https://` or `http://`
    /// - `s3_bucket` must not be empty
    /// - `s3_access_key` must not be empty
    /// - `s3_secret_key` must not be empty
    pub fn validate(&self) -> Result<(), String> {
        if !self.s3_endpoint.starts_with("https://") && !self.s3_endpoint.starts_with("http://") {
            return Err("s3_endpoint must start with https:// or http://".to_string());
        }
        if self.s3_bucket.is_empty() {
            return Err("s3_bucket must not be empty".to_string());
        }
        if self.s3_access_key.is_empty() {
            return Err("s3_access_key must not be empty".to_string());
        }
        if self.s3_secret_key.is_empty() {
            return Err("s3_secret_key must not be empty".to_string());
        }
        Ok(())
    }
}

/// Scope for storage quotas — either per-org or per-project.
// TODO: Replace with typed OrgId/ProjectId from syfrah-core once #1178 lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum QuotaScope {
    /// Quota applies to an entire organisation.
    Org { org_id: String },
    /// Quota applies to a specific project within an organisation.
    Project { org_id: String, project_id: String },
}

impl std::fmt::Display for QuotaScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuotaScope::Org { org_id } => write!(f, "Org({org_id})"),
            QuotaScope::Project { org_id, project_id } => {
                write!(f, "Project({project_id}@{org_id})")
            }
        }
    }
}

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

    // -- VPC Attach/Detach --
    AttachVpc {
        vpc: String,
        project: String,
    },
    DetachVpc {
        vpc: String,
        project: String,
    },

    // -- Environment mutations --
    ExtendEnv {
        name: String,
        project: String,
        org: String,
        ttl_seconds: u64,
    },
    UpdateEnv {
        name: String,
        project: String,
        org: String,
        deletion_protection: Option<bool>,
    },

    // -- Route Table --
    CreateRouteTable {
        name: String,
        vpc: String,
    },
    DeleteRouteTable {
        name: String,
        vpc: Option<String>,
    },
    AssociateRouteTable {
        table: String,
        subnet: String,
    },
    DisassociateRouteTable {
        subnet: String,
    },

    // -- Routes --
    AddRoute {
        vpc: String,
        table: Option<String>,
        destination: String,
        target: String,
        priority: Option<u32>,
    },
    DeleteRoute {
        vpc: String,
        table: Option<String>,
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
        fabric_ipv6: String,
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
    UpdateHypervisorCapacity {
        name: String,
        allocatable_vcpus: u32,
        allocatable_memory_mb: u64,
        used_vcpus: u32,
        used_memory_mb: u64,
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

    // -- Storage (ADR-006 §16) --
    // TODO: Replace String IDs with typed VolumeId/SnapshotId/VmId/HypervisorId
    // from syfrah-core once #1178 lands.
    /// Create a new volume. Raft validates quota and name uniqueness within env.
    CreateVolume {
        id: String,
        name: String,
        size_gb: u32,
        org_id: String,
        project_id: String,
        env_id: String,
        volume_type: VolumeType,
    },
    /// Mark a volume for deletion (tombstone). Volume must be Available (not attached).
    /// With `cascade: true`, all snapshots referencing this volume are deleted first.
    DeleteVolume {
        volume_id: String,
        /// If true, delete all snapshots derived from this volume before deleting.
        #[serde(default)]
        cascade: bool,
        /// Timestamp (seconds since epoch) when the delete was requested.
        /// Must be set by the caller so every Raft replica uses the same value
        /// (calling `SystemTime::now()` inside `apply_command` would diverge).
        deleted_at: u64,
    },
    /// Attach a volume to a VM on a specific hypervisor. Enforces the
    /// single-writer invariant — the volume must be Available.
    /// Increments `placement_generation` for fencing.
    VolumeAttach {
        volume_id: String,
        vm_id: String,
        hypervisor_id: String,
    },
    /// Detach a volume from its current VM. Volume must be Attached.
    VolumeDetach {
        volume_id: String,
    },
    /// Resize a volume (grow only, no shrink in v1). Volume must be Available.
    ResizeVolume {
        volume_id: String,
        new_size_gb: u32,
    },
    /// Record a crash-consistent snapshot. Increments SST refcounts.
    CreateSnapshot {
        id: String,
        source_volume_id: String,
        sst_files: Vec<String>,
        wal_position: u64,
    },
    /// Delete a snapshot. Decrements SST refcounts; GC reclaims unreachable SSTs.
    DeleteSnapshot {
        snapshot_id: String,
    },
    /// Restore a snapshot into a new volume with a fresh generation.
    RestoreSnapshot {
        snapshot_id: String,
        new_volume_id: String,
        new_volume_name: String,
    },
    /// Set per-region S3 storage configuration (replicated to all nodes).
    /// NOTE: The encryption_passphrase is NOT included — it is stored locally
    /// on each hypervisor with 0600 permissions, never replicated via Raft.
    /// The S3 credentials (access_key, secret_key) ARE stored in Raft so that
    /// every node in the region can reach the bucket.
    SetStorageConfig {
        region: String,
        config: Box<StorageConfig>,
    },
    /// Set storage quotas for an org or project.
    SetStorageQuota {
        scope: QuotaScope,
        max_volumes: u32,
        max_total_gb: u64,
        max_snapshots: u32,
    },

    /// Purge volume tombstones older than `max_age_secs`.
    /// Called periodically by a background task to clean up old Deleted records.
    PurgeTombstones {
        /// Current timestamp (seconds since epoch).
        now: u64,
        /// Maximum age in seconds before a tombstone is purged.
        max_age_secs: u64,
    },

    /// Reschedule a volume from one hypervisor to another.
    ///
    /// On VM reschedule, volumes attached to the VM must be migrated:
    /// 1. Source stops ZeroFS (flush), Raft increments placement_generation.
    /// 2. Target reconciler detects volume, starts ZeroFS with new gen prefix.
    /// 3. Source self-fences on recovery by detecting stale generation.
    ///
    /// The volume remains in `Attached` state throughout — only the
    /// hypervisor assignment and generation change (zero-copy migration).
    RescheduleVolume {
        volume_id: String,
        from_hypervisor: String,
        to_hypervisor: String,
        /// New VM ID on the target hypervisor (may differ if the VM was re-created).
        new_vm_id: String,
    },

    /// Commit a manifest pointer for a volume (ADR-006 §12b).
    ///
    /// Validates:
    /// - `generation` matches the volume's current `placement_generation`
    /// - `manifest_version` == last committed version + 1 (strict sequential)
    /// - `published_by` matches the hypervisor the volume is attached to
    CommitManifest {
        volume_id: String,
        generation: u64,
        manifest_version: u64,
        s3_key: String,
        published_by: String,
    },

    // -- Composite Transaction --
    /// Atomic batch of commands applied in a single Raft log entry.
    /// All sub-commands succeed or all fail. Used for placement transactions
    /// that must atomically AllocateIp + CreateNic + PlaceVm.
    Composite {
        commands: Vec<StateMachineCommand>,
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
            Self::AttachVpc { vpc, project } => write!(f, "AttachVpc({vpc}@{project})"),
            Self::DetachVpc { vpc, project } => write!(f, "DetachVpc({vpc}@{project})"),
            Self::CreateRouteTable { name, vpc } => write!(f, "CreateRouteTable({name}@{vpc})"),
            Self::UpdateHypervisorCapacity { name, .. } => {
                write!(f, "UpdateHypervisorCapacity({name})")
            }
            Self::CreateVolume { id, name, .. } => write!(f, "CreateVolume({id}, {name})"),
            Self::DeleteVolume {
                volume_id, cascade, ..
            } => {
                write!(f, "DeleteVolume({volume_id}, cascade={cascade})")
            }
            Self::VolumeAttach {
                volume_id, vm_id, ..
            } => write!(f, "VolumeAttach({volume_id}→{vm_id})"),
            Self::VolumeDetach { volume_id } => write!(f, "VolumeDetach({volume_id})"),
            Self::ResizeVolume {
                volume_id,
                new_size_gb,
            } => write!(f, "ResizeVolume({volume_id}, {new_size_gb}GB)"),
            Self::CreateSnapshot { id, .. } => write!(f, "CreateSnapshot({id})"),
            Self::DeleteSnapshot { snapshot_id } => write!(f, "DeleteSnapshot({snapshot_id})"),
            Self::RestoreSnapshot {
                snapshot_id,
                new_volume_id,
                ..
            } => write!(f, "RestoreSnapshot({snapshot_id}→{new_volume_id})"),
            Self::SetStorageConfig { region, .. } => write!(f, "SetStorageConfig({region})"),
            Self::SetStorageQuota { scope, .. } => write!(f, "SetStorageQuota({scope})"),
            Self::PurgeTombstones { max_age_secs, .. } => {
                write!(f, "PurgeTombstones(ttl={max_age_secs}s)")
            }
            Self::RescheduleVolume {
                volume_id,
                from_hypervisor,
                to_hypervisor,
                ..
            } => write!(
                f,
                "RescheduleVolume({volume_id}: {from_hypervisor}->{to_hypervisor})"
            ),
            Self::CommitManifest {
                volume_id,
                manifest_version,
                ..
            } => write!(f, "CommitManifest({volume_id}, v{manifest_version})"),
            Self::Composite { commands } => write!(f, "Composite({})", commands.len()),
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
    /// Storage configuration for a region.
    StorageConfig(Box<StorageConfig>),
    /// Composite transaction result — contains results from each sub-command.
    Composite(Vec<StateMachineResponse>),
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
            StateMachineCommand::AttachVpc {
                vpc: "v".into(),
                project: "p".into(),
            },
            StateMachineCommand::DetachVpc {
                vpc: "v".into(),
                project: "p".into(),
            },
            StateMachineCommand::ExtendEnv {
                name: "dev".into(),
                project: "b".into(),
                org: "a".into(),
                ttl_seconds: 7200,
            },
            StateMachineCommand::UpdateEnv {
                name: "dev".into(),
                project: "b".into(),
                org: "a".into(),
                deletion_protection: Some(true),
            },
            StateMachineCommand::CreateRouteTable {
                name: "rt".into(),
                vpc: "v".into(),
            },
            StateMachineCommand::DeleteRouteTable {
                name: "rt".into(),
                vpc: Some("v".into()),
            },
            StateMachineCommand::AssociateRouteTable {
                table: "rt".into(),
                subnet: "s".into(),
            },
            StateMachineCommand::DisassociateRouteTable { subnet: "s".into() },
            StateMachineCommand::AddRoute {
                vpc: "v".into(),
                table: None,
                destination: "0.0.0.0/0".into(),
                target: "nat".into(),
                priority: Some(100),
            },
            StateMachineCommand::DeleteRoute {
                vpc: "v".into(),
                table: None,
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
                fabric_ipv6: "fd00::1".into(),
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
            StateMachineCommand::UpdateHypervisorCapacity {
                name: "hv1".into(),
                allocatable_vcpus: 16,
                allocatable_memory_mb: 65536,
                used_vcpus: 4,
                used_memory_mb: 8192,
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
            // -- Storage --
            StateMachineCommand::CreateVolume {
                id: "vol-01".into(),
                name: "pgdata".into(),
                size_gb: 100,
                org_id: "acme".into(),
                project_id: "myapp".into(),
                env_id: "prod".into(),
                volume_type: VolumeType::Data,
            },
            StateMachineCommand::DeleteVolume {
                volume_id: "vol-01".into(),
                cascade: false,
                deleted_at: 1700000000,
            },
            StateMachineCommand::VolumeAttach {
                volume_id: "vol-01".into(),
                vm_id: "vm1".into(),
                hypervisor_id: "hv1".into(),
            },
            StateMachineCommand::VolumeDetach {
                volume_id: "vol-01".into(),
            },
            StateMachineCommand::ResizeVolume {
                volume_id: "vol-01".into(),
                new_size_gb: 200,
            },
            StateMachineCommand::CreateSnapshot {
                id: "snap-01".into(),
                source_volume_id: "vol-01".into(),
                sst_files: vec!["sst-001".into(), "sst-002".into()],
                wal_position: 42,
            },
            StateMachineCommand::DeleteSnapshot {
                snapshot_id: "snap-01".into(),
            },
            StateMachineCommand::RestoreSnapshot {
                snapshot_id: "snap-01".into(),
                new_volume_id: "vol-02".into(),
                new_volume_name: "pgdata-restored".into(),
            },
            StateMachineCommand::SetStorageConfig {
                region: "eu-west".into(),
                config: Box::new(StorageConfig {
                    s3_endpoint: "https://s3.par.io.cloud.ovh.net".into(),
                    s3_bucket: "syfrah-storage-eu-west".into(),
                    s3_access_key: "AKID".into(),
                    s3_secret_key: "secret".into(),
                    cache_disk_path: "/dev/nvme1n1".into(),
                    cache_disk_size_gb: 200,
                    cache_memory_size_gb: 8,
                }),
            },
            StateMachineCommand::SetStorageQuota {
                scope: QuotaScope::Org {
                    org_id: "acme".into(),
                },
                max_volumes: 50,
                max_total_gb: 10000,
                max_snapshots: 200,
            },
            StateMachineCommand::RescheduleVolume {
                volume_id: "vol-01".into(),
                from_hypervisor: "hv1".into(),
                to_hypervisor: "hv2".into(),
                new_vm_id: "vm-new".into(),
            },
            StateMachineCommand::CommitManifest {
                volume_id: "vol-01".into(),
                generation: 1,
                manifest_version: 1,
                s3_key: "manifests/vol-01/v1.json".into(),
                published_by: "hv1".into(),
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
            StateMachineResponse::StorageConfig(Box::new(StorageConfig {
                s3_endpoint: "https://s3.example.com".into(),
                s3_bucket: "bucket".into(),
                s3_access_key: "AKID".into(),
                s3_secret_key: "secret".into(),
                cache_disk_path: "/dev/nvme0".into(),
                cache_disk_size_gb: 100,
                cache_memory_size_gb: 4,
            })),
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

    #[test]
    fn storage_config_validate_valid() {
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn storage_config_validate_http_endpoint() {
        let config = StorageConfig {
            s3_endpoint: "http://minio:9000".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        assert!(config.validate().is_ok());
    }

    #[test]
    fn storage_config_validate_rejects_ftp_endpoint() {
        let config = StorageConfig {
            s3_endpoint: "ftp://bad.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("s3_endpoint must start with https:// or http://"));
    }

    #[test]
    fn storage_config_validate_rejects_empty_bucket() {
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        assert!(config.validate().unwrap_err().contains("s3_bucket"));
    }

    #[test]
    fn storage_config_validate_rejects_empty_access_key() {
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        assert!(config.validate().unwrap_err().contains("s3_access_key"));
    }

    #[test]
    fn storage_config_validate_rejects_empty_secret_key() {
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        assert!(config.validate().unwrap_err().contains("s3_secret_key"));
    }

    #[test]
    fn storage_config_serde_roundtrip() {
        let config = StorageConfig {
            s3_endpoint: "https://s3.example.com".into(),
            s3_bucket: "bucket".into(),
            s3_access_key: "AKID".into(),
            s3_secret_key: "secret".into(),
            cache_disk_path: "/dev/nvme0".into(),
            cache_disk_size_gb: 100,
            cache_memory_size_gb: 4,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: StorageConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }
}
