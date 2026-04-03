use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// ID newtypes — thin wrappers around Uuid.
// When #1178 lands with its own ID types, these may be consolidated.
// ---------------------------------------------------------------------------

/// Unique identifier for a virtual machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VmId(pub Uuid);

/// Unique identifier for a hypervisor node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HypervisorId(pub Uuid);

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

    // Helpers ---------------------------------------------------------------

    fn vm_id() -> VmId {
        VmId(Uuid::from_u128(1))
    }

    fn hypervisor_id() -> HypervisorId {
        HypervisorId(Uuid::from_u128(2))
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
    // NOTE: Resizing requires knowledge of the "new size" field which is not yet
    // in VolumeDesiredState (ADR-006 mentions "Available with new size"). Until
    // that field is added, resizing is not distinguishable from Available. We
    // add a placeholder test documenting this gap.

    #[test]
    fn resizing_not_yet_distinguishable() {
        // When a resize_target_bytes field is added to VolumeDesiredState::Available,
        // this test should be updated to assert VolumeReportedState::Resizing.
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
    // Error requires a retry counter or reconciliation-failure flag which is
    // outside the scope of desired/observed state alone. The derive function
    // does not currently return Error — that will be handled by the reconciler
    // layer. We document that here.

    #[test]
    fn error_state_requires_reconciler_context() {
        // Verify that VolumeReportedState::Error variant exists and can be
        // constructed (it will be set by the reconciler, not by derive_reported_state).
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
        // Edge: desired Deleted but NBD is still up (shouldn't normally happen
        // per transition rules, but the derive function must handle it).
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
