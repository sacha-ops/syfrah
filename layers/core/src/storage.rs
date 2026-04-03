use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Newtype identifiers
// ---------------------------------------------------------------------------

/// Unique identifier for a volume. Format: `vol-{ulid}`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeId(pub String);

impl fmt::Display for VolumeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a snapshot. Format: `snap-{ulid}`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotId(pub String);

impl fmt::Display for SnapshotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a VM. Format: `vm-{ulid}`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmId(pub String);

impl fmt::Display for VmId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a hypervisor. Format: `hv-{ulid}`.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct HypervisorId(pub String);

impl fmt::Display for HypervisorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for an organization.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct OrgId(pub String);

impl fmt::Display for OrgId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for a project.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectId(pub String);

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Unique identifier for an environment.
#[derive(Debug, Clone, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub struct EnvId(pub String);

impl fmt::Display for EnvId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

/// Whether a volume is a root volume (tied to a VM) or a data volume
/// (independent lifecycle).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeType {
    /// Root volume: created automatically with a VM, deleted with the VM.
    Root,
    /// Data volume: created independently, lifecycle is not tied to any VM.
    Data,
}

/// Minimal desired-state enum so the `Volume` struct compiles.
/// The full desired/observed/reported state model is defined in #1179.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeDesiredState {
    Available,
    Attached,
    Deleted,
}

/// A persistent block volume backed by S3 via ZeroFS.
///
/// See ADR-006 §4 for the full resource model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Globally unique identifier. Format: `vol-{ulid}`.
    pub id: VolumeId,
    /// Human-readable name. Unique within an environment.
    pub name: String,
    /// Size in gigabytes.
    pub size_gb: u32,
    /// Current desired state in the volume lifecycle.
    pub desired_state: VolumeDesiredState,
    /// The VM this volume is attached to, if any.
    pub attached_to: Option<VmId>,
    /// The hypervisor where the NBD device is currently active.
    pub hypervisor_id: Option<HypervisorId>,
    /// S3 key prefix for this volume's data.
    pub s3_prefix: String,
    /// Encryption key identifier.
    pub encryption_key_id: String,
    /// Snapshots taken from this volume.
    pub snapshot_ids: Vec<SnapshotId>,
    /// Organization this volume belongs to.
    pub org_id: OrgId,
    /// Project this volume belongs to.
    pub project_id: ProjectId,
    /// Environment this volume belongs to.
    pub env_id: EnvId,
    /// Placement generation — incremented on attach/detach/reschedule.
    pub placement_generation: u64,
    /// Root or data volume.
    pub volume_type: VolumeType,
    /// Unix timestamp (seconds) when the volume was created.
    pub created_at: u64,
    /// Unix timestamp (seconds) when the volume was last updated.
    pub updated_at: u64,
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

/// Lifecycle state of a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotState {
    /// ZeroFS is flushing pending writes and recording the SST file list.
    Creating,
    /// Snapshot is available for restore.
    Available,
    /// SST file references are being removed.
    Deleting,
    /// Terminal state.
    Deleted,
}

/// A crash-consistent point-in-time snapshot of a volume.
///
/// See ADR-006 §6 for the full snapshot model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Globally unique identifier. Format: `snap-{ulid}`.
    pub id: SnapshotId,
    /// The volume this snapshot was taken from.
    pub source_volume_id: VolumeId,
    /// Current state.
    pub state: SnapshotState,
    /// Size in gigabytes (matches source volume at snapshot time).
    pub size_gb: u32,
    /// S3 prefix where snapshot metadata is stored.
    pub s3_prefix: String,
    /// Organization (inherited from source volume).
    pub org_id: OrgId,
    /// Project (inherited from source volume).
    pub project_id: ProjectId,
    /// Environment (inherited from source volume).
    pub env_id: EnvId,
    /// Unix timestamp (seconds) when the snapshot was created.
    pub created_at: u64,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_volume() -> Volume {
        Volume {
            id: VolumeId("vol-01JA0000000000000000000000".into()),
            name: "my-volume".into(),
            size_gb: 100,
            desired_state: VolumeDesiredState::Available,
            attached_to: None,
            hypervisor_id: None,
            s3_prefix: "volumes/vol-01JA0000000000000000000000/gen-1/".into(),
            encryption_key_id: "key-default".into(),
            snapshot_ids: vec![SnapshotId("snap-01JA0000000000000000000001".into())],
            org_id: OrgId("org-acme".into()),
            project_id: ProjectId("proj-web".into()),
            env_id: EnvId("env-prod".into()),
            placement_generation: 1,
            volume_type: VolumeType::Root,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        }
    }

    fn sample_snapshot() -> Snapshot {
        Snapshot {
            id: SnapshotId("snap-01JA0000000000000000000001".into()),
            source_volume_id: VolumeId("vol-01JA0000000000000000000000".into()),
            state: SnapshotState::Available,
            size_gb: 100,
            s3_prefix: "snapshots/snap-01JA0000000000000000000001/".into(),
            org_id: OrgId("org-acme".into()),
            project_id: ProjectId("proj-web".into()),
            env_id: EnvId("env-prod".into()),
            created_at: 1_700_000_000,
        }
    }

    #[test]
    fn volume_serde_roundtrip() {
        let vol = sample_volume();
        let json = serde_json::to_string(&vol).unwrap();
        let parsed: Volume = serde_json::from_str(&json).unwrap();
        assert_eq!(vol, parsed);
    }

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = sample_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        let parsed: Snapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(snap, parsed);
    }

    #[test]
    fn volume_type_serde_roundtrip() {
        for vt in [VolumeType::Root, VolumeType::Data] {
            let json = serde_json::to_string(&vt).unwrap();
            let parsed: VolumeType = serde_json::from_str(&json).unwrap();
            assert_eq!(vt, parsed);
        }
    }

    #[test]
    fn snapshot_state_serde_roundtrip() {
        for state in [
            SnapshotState::Creating,
            SnapshotState::Available,
            SnapshotState::Deleting,
            SnapshotState::Deleted,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: SnapshotState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn volume_desired_state_serde_roundtrip() {
        for state in [
            VolumeDesiredState::Available,
            VolumeDesiredState::Attached,
            VolumeDesiredState::Deleted,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: VolumeDesiredState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, parsed);
        }
    }

    #[test]
    fn volume_with_attachment() {
        let mut vol = sample_volume();
        vol.desired_state = VolumeDesiredState::Attached;
        vol.attached_to = Some(VmId("vm-01JA0000000000000000000099".into()));
        vol.hypervisor_id = Some(HypervisorId("hv-01JA0000000000000000000042".into()));

        let json = serde_json::to_string(&vol).unwrap();
        let parsed: Volume = serde_json::from_str(&json).unwrap();
        assert_eq!(vol, parsed);
    }
}
