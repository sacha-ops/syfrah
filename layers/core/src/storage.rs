use serde::{Deserialize, Serialize};

use crate::ids::{EnvId, HypervisorId, OrgId, ProjectId, SnapshotId, VmId, VolumeId};

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
// Desired state — stored in Raft (control plane truth)
// ---------------------------------------------------------------------------

/// The operator's intent for a volume (ADR-006 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeDesiredState {
    /// Volume should exist and be available for attachment.
    Available,
    /// Volume should be attached to the specified VM on the specified hypervisor.
    AttachedTo {
        vm_id: VmId,
        hypervisor_id: HypervisorId,
    },
    /// Volume should be deleted. All data removed from S3.
    Deleted,
}

/// A versioned desired-state record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeDesiredRecord {
    /// The target state.
    pub target: VolumeDesiredState,
    /// Monotonically increasing generation counter — bumped on every desired-state change.
    pub generation: u64,
}

// ---------------------------------------------------------------------------
// Observed state — reported by the hypervisor agent (Forge)
// ---------------------------------------------------------------------------

/// Real-time observation of a volume on a particular hypervisor (ADR-006 §5).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeObservedState {
    /// Whether the NBD device is connected and serving I/O.
    pub nbd_connected: bool,
    /// Bytes currently in the local cache (memory + SSD).
    pub cache_bytes: u64,
    /// Bytes written but not yet flushed to S3 WAL.
    pub dirty_bytes: u64,
    /// Whether Cloud Hypervisor has the device attached as virtio-block.
    pub ch_attached: bool,
    /// Timestamp of last observation (Unix epoch seconds).
    pub last_observed: u64,
}

// ---------------------------------------------------------------------------
// Reported state — derived, exposed to API / CLI
// ---------------------------------------------------------------------------

/// The human-/API-visible state of a volume, derived from desired + observed
/// state (ADR-006 §5 mapping table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VolumeReportedState {
    /// Desired: Available, Observed: not yet initialized on S3.
    Creating,
    /// Desired: Available, Observed: NBD disconnected, no dirty data.
    Available,
    /// Desired: Attached, Observed: NBD connecting or CH not yet attached.
    Attaching,
    /// Desired: Attached, Observed: NBD connected AND CH attached.
    Attached,
    /// Desired: Available, Observed: flushing cache / disconnecting NBD.
    Detaching,
    /// Desired: Available with new size, Observed: resize in progress.
    Resizing,
    /// Volume is being migrated between zones (S3-to-S3 copy in progress).
    Migrating,
    /// Desired: Deleted, Observed: S3 objects being removed.
    Deleting,
    /// Desired: Deleted, Observed: all S3 data removed.
    Deleted,
    /// Desired != Observed AND reconciliation failed after N retries.
    Error,
}

// ---------------------------------------------------------------------------
// derive_reported_state — the core mapping function
// ---------------------------------------------------------------------------

/// Derive the reported state from desired + observed state.
///
/// Implements the mapping table in ADR-006 §5.
///
/// # Arguments
/// * `desired` — the operator's intended state (from Raft).
/// * `observed` — the hypervisor's latest observation (from Forge).
///
/// # Returns
/// The [`VolumeReportedState`] that should be exposed to the API / CLI.
pub fn derive_reported_state(
    desired: &VolumeDesiredState,
    observed: &VolumeObservedState,
) -> VolumeReportedState {
    match desired {
        // ----- Desired: Available -------------------------------------------
        VolumeDesiredState::Available => {
            if observed.nbd_connected || observed.ch_attached {
                // Still connected — we are detaching (flushing cache, disconnecting NBD).
                VolumeReportedState::Detaching
            } else if observed.dirty_bytes > 0 {
                // Cache is still flushing (dirty data remains) but devices already
                // disconnected — still in the detach/flush cycle.
                VolumeReportedState::Detaching
            } else if observed.last_observed == 0 {
                // Never observed — volume has not been initialised on S3 yet.
                VolumeReportedState::Creating
            } else {
                // Clean, disconnected, previously observed — truly available.
                VolumeReportedState::Available
            }
        }

        // ----- Desired: AttachedTo ------------------------------------------
        VolumeDesiredState::AttachedTo { .. } => {
            if observed.nbd_connected && observed.ch_attached {
                // Fully attached.
                VolumeReportedState::Attached
            } else {
                // NBD connecting or CH not yet attached — in progress.
                VolumeReportedState::Attaching
            }
        }

        // ----- Desired: Deleted ---------------------------------------------
        VolumeDesiredState::Deleted => {
            if observed.last_observed == 0
                && !observed.nbd_connected
                && !observed.ch_attached
                && observed.cache_bytes == 0
                && observed.dirty_bytes == 0
            {
                // All data removed (or never existed) — fully deleted.
                VolumeReportedState::Deleted
            } else {
                // S3 objects still being removed.
                VolumeReportedState::Deleting
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helpers (Soren's volume/snapshot helpers) ---------------------------

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

    // -- Helpers (Ren's state-test helpers) ----------------------------------

    fn vm_id() -> VmId {
        VmId("vm-01JA0000000000000000000099".into())
    }

    fn hypervisor_id() -> HypervisorId {
        HypervisorId("hv-01JA0000000000000000000042".into())
    }

    /// An observed state that has been seen at least once but is fully idle.
    fn idle_observed() -> VolumeObservedState {
        VolumeObservedState {
            nbd_connected: false,
            cache_bytes: 0,
            dirty_bytes: 0,
            ch_attached: false,
            last_observed: 1_000,
        }
    }

    /// A fully-attached observed state.
    fn attached_observed() -> VolumeObservedState {
        VolumeObservedState {
            nbd_connected: true,
            cache_bytes: 4096,
            dirty_bytes: 0,
            ch_attached: true,
            last_observed: 2_000,
        }
    }

    // -- Soren's tests: Volume/Snapshot serde roundtrips ---------------------

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
    fn volume_with_attachment() {
        let mut vol = sample_volume();
        vol.desired_state = VolumeDesiredState::AttachedTo {
            vm_id: vm_id(),
            hypervisor_id: hypervisor_id(),
        };
        vol.attached_to = Some(VmId("vm-01JA0000000000000000000099".into()));
        vol.hypervisor_id = Some(HypervisorId("hv-01JA0000000000000000000042".into()));

        let json = serde_json::to_string(&vol).unwrap();
        let parsed: Volume = serde_json::from_str(&json).unwrap();
        assert_eq!(vol, parsed);
    }

    // -- Ren's tests: derive_reported_state ---------------------------------

    // 1. Creating -----------------------------------------------------------

    #[test]
    fn creating_when_never_observed() {
        let desired = VolumeDesiredState::Available;
        let observed = VolumeObservedState::default(); // last_observed == 0
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Creating
        );
    }

    // 2. Available ----------------------------------------------------------

    #[test]
    fn available_when_idle_and_observed() {
        let desired = VolumeDesiredState::Available;
        let observed = idle_observed();
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Available
        );
    }

    // 3. Attaching ----------------------------------------------------------

    #[test]
    fn attaching_when_nbd_not_connected() {
        let desired = VolumeDesiredState::AttachedTo {
            vm_id: vm_id(),
            hypervisor_id: hypervisor_id(),
        };
        let observed = idle_observed(); // nbd_connected = false
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Attaching
        );
    }

    #[test]
    fn attaching_when_nbd_connected_but_ch_not_attached() {
        let desired = VolumeDesiredState::AttachedTo {
            vm_id: vm_id(),
            hypervisor_id: hypervisor_id(),
        };
        let observed = VolumeObservedState {
            nbd_connected: true,
            ch_attached: false,
            last_observed: 1_500,
            ..Default::default()
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Attaching
        );
    }

    #[test]
    fn attaching_when_ch_attached_but_nbd_not_connected() {
        let desired = VolumeDesiredState::AttachedTo {
            vm_id: vm_id(),
            hypervisor_id: hypervisor_id(),
        };
        let observed = VolumeObservedState {
            nbd_connected: false,
            ch_attached: true,
            last_observed: 1_500,
            ..Default::default()
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Attaching
        );
    }

    // 4. Attached -----------------------------------------------------------

    #[test]
    fn attached_when_fully_connected() {
        let desired = VolumeDesiredState::AttachedTo {
            vm_id: vm_id(),
            hypervisor_id: hypervisor_id(),
        };
        let observed = attached_observed();
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Attached
        );
    }

    // 5. Detaching ----------------------------------------------------------

    #[test]
    fn detaching_when_desired_available_but_nbd_still_connected() {
        let desired = VolumeDesiredState::Available;
        let observed = VolumeObservedState {
            nbd_connected: true,
            ch_attached: false,
            last_observed: 3_000,
            ..Default::default()
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Detaching
        );
    }

    #[test]
    fn detaching_when_desired_available_but_ch_still_attached() {
        let desired = VolumeDesiredState::Available;
        let observed = VolumeObservedState {
            nbd_connected: false,
            ch_attached: true,
            last_observed: 3_000,
            ..Default::default()
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Detaching
        );
    }

    #[test]
    fn detaching_when_dirty_bytes_remain() {
        let desired = VolumeDesiredState::Available;
        let observed = VolumeObservedState {
            nbd_connected: false,
            ch_attached: false,
            dirty_bytes: 512,
            last_observed: 3_000,
            ..Default::default()
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Detaching
        );
    }

    // 6. Resizing -----------------------------------------------------------

    #[test]
    fn resizing_not_yet_distinguishable() {
        let desired = VolumeDesiredState::Available;
        let observed = idle_observed();
        // Currently maps to Available (no resize field yet).
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Available
        );
    }

    // 7. Deleting -----------------------------------------------------------

    #[test]
    fn deleting_when_data_still_present() {
        let desired = VolumeDesiredState::Deleted;
        let observed = VolumeObservedState {
            nbd_connected: false,
            ch_attached: false,
            cache_bytes: 1024,
            dirty_bytes: 0,
            last_observed: 4_000,
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Deleting
        );
    }

    #[test]
    fn deleting_when_still_observed() {
        let desired = VolumeDesiredState::Deleted;
        let observed = idle_observed(); // last_observed != 0
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Deleting
        );
    }

    // 8. Deleted ------------------------------------------------------------

    #[test]
    fn deleted_when_fully_cleaned() {
        let desired = VolumeDesiredState::Deleted;
        let observed = VolumeObservedState::default(); // all zeros
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Deleted
        );
    }

    // 9. Error --------------------------------------------------------------

    #[test]
    fn error_state_requires_reconciler_context() {
        let err = VolumeReportedState::Error;
        assert_eq!(err, VolumeReportedState::Error);
    }

    // Edge cases ------------------------------------------------------------

    #[test]
    fn default_observed_state_is_all_zeros() {
        let obs = VolumeObservedState::default();
        assert!(!obs.nbd_connected);
        assert_eq!(obs.cache_bytes, 0);
        assert_eq!(obs.dirty_bytes, 0);
        assert!(!obs.ch_attached);
        assert_eq!(obs.last_observed, 0);
    }

    #[test]
    fn serde_roundtrip_desired_state() {
        let states = vec![
            VolumeDesiredState::Available,
            VolumeDesiredState::AttachedTo {
                vm_id: vm_id(),
                hypervisor_id: hypervisor_id(),
            },
            VolumeDesiredState::Deleted,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let parsed: VolumeDesiredState = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, state);
        }
    }

    #[test]
    fn serde_roundtrip_observed_state() {
        let obs = attached_observed();
        let json = serde_json::to_string(&obs).unwrap();
        let parsed: VolumeObservedState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, obs);
    }

    #[test]
    fn serde_roundtrip_reported_state() {
        let states = vec![
            VolumeReportedState::Creating,
            VolumeReportedState::Available,
            VolumeReportedState::Attaching,
            VolumeReportedState::Attached,
            VolumeReportedState::Detaching,
            VolumeReportedState::Resizing,
            VolumeReportedState::Migrating,
            VolumeReportedState::Deleting,
            VolumeReportedState::Deleted,
            VolumeReportedState::Error,
        ];
        for state in &states {
            let json = serde_json::to_string(state).unwrap();
            let parsed: VolumeReportedState = serde_json::from_str(&json).unwrap();
            assert_eq!(&parsed, state);
        }
    }

    #[test]
    fn serde_roundtrip_desired_record() {
        let record = VolumeDesiredRecord {
            target: VolumeDesiredState::AttachedTo {
                vm_id: vm_id(),
                hypervisor_id: hypervisor_id(),
            },
            generation: 42,
        };
        let json = serde_json::to_string(&record).unwrap();
        let parsed: VolumeDesiredRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, record);
    }

    #[test]
    fn deleting_when_nbd_still_connected() {
        let desired = VolumeDesiredState::Deleted;
        let observed = VolumeObservedState {
            nbd_connected: true,
            ch_attached: false,
            cache_bytes: 0,
            dirty_bytes: 0,
            last_observed: 5_000,
        };
        assert_eq!(
            derive_reported_state(&desired, &observed),
            VolumeReportedState::Deleting
        );
    }
}
