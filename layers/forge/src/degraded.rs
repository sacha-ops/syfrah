//! Control-degraded policy.
//!
//! When the control plane is unreachable or projection stale > 5 min:
//! - Reads: allowed (last known state)
//! - Reconcile existing: allowed
//! - Creates: DENIED
//! - Deletes: DENIED
//! - Start/stop: allowed
//!
//! In bootstrap mode (no control plane), this is always "connected."

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Staleness threshold: 5 minutes.
const STALE_THRESHOLD: Duration = Duration::from_secs(5 * 60);

/// Operation categories for the degraded policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    /// Read operations (list, get).
    Read,
    /// Reconcile existing resources.
    Reconcile,
    /// Create new resources.
    Create,
    /// Delete resources.
    Delete,
    /// Start/stop existing resources.
    StartStop,
}

/// Control plane connection state.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ControlPlaneState {
    /// Connected and projection is fresh.
    Connected,
    /// Control plane unreachable or projection stale.
    Degraded,
    /// No control plane exists (bootstrap mode).
    Bootstrap,
}

/// Degraded policy snapshot.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DegradedStatus {
    pub state: ControlPlaneState,
    pub reads_allowed: bool,
    pub reconcile_allowed: bool,
    pub creates_allowed: bool,
    pub deletes_allowed: bool,
    pub start_stop_allowed: bool,
    pub last_projection_age_secs: Option<u64>,
}

/// Controller for the degraded policy.
pub struct DegradedController {
    /// Whether we are in bootstrap mode (no control plane).
    bootstrap_mode: AtomicBool,
    /// Last time we received a fresh projection from the control plane.
    last_projection_at: Mutex<Option<Instant>>,
    /// Whether the control plane is currently reachable.
    control_plane_reachable: AtomicBool,
}

impl DegradedController {
    /// Create a new controller in bootstrap mode (no control plane).
    pub fn new_bootstrap() -> Self {
        Self {
            bootstrap_mode: AtomicBool::new(true),
            last_projection_at: Mutex::new(None),
            control_plane_reachable: AtomicBool::new(true),
        }
    }

    /// Get the current control plane state.
    pub fn state(&self) -> ControlPlaneState {
        if self.bootstrap_mode.load(Ordering::SeqCst) {
            return ControlPlaneState::Bootstrap;
        }
        if !self.control_plane_reachable.load(Ordering::SeqCst) {
            return ControlPlaneState::Degraded;
        }
        if let Some(last) = *self.last_projection_at.lock().unwrap() {
            if last.elapsed() > STALE_THRESHOLD {
                return ControlPlaneState::Degraded;
            }
        }
        ControlPlaneState::Connected
    }

    /// Check if a given operation is allowed under the current policy.
    pub fn is_allowed(&self, op: OperationKind) -> bool {
        match self.state() {
            ControlPlaneState::Connected | ControlPlaneState::Bootstrap => true,
            ControlPlaneState::Degraded => matches!(
                op,
                OperationKind::Read | OperationKind::Reconcile | OperationKind::StartStop
            ),
        }
    }

    /// Record a fresh projection from the control plane.
    pub fn record_projection(&self) {
        *self.last_projection_at.lock().unwrap() = Some(Instant::now());
    }

    /// Mark control plane as unreachable.
    pub fn mark_unreachable(&self) {
        self.control_plane_reachable.store(false, Ordering::SeqCst);
    }

    /// Mark control plane as reachable.
    pub fn mark_reachable(&self) {
        self.control_plane_reachable.store(true, Ordering::SeqCst);
    }

    /// Get a snapshot of the degraded status.
    pub fn status(&self) -> DegradedStatus {
        let state = self.state();
        let last_age = self
            .last_projection_at
            .lock()
            .unwrap()
            .map(|t| t.elapsed().as_secs());

        let (reads, reconcile, creates, deletes, start_stop) = match state {
            ControlPlaneState::Connected | ControlPlaneState::Bootstrap => {
                (true, true, true, true, true)
            }
            ControlPlaneState::Degraded => (true, true, false, false, true),
        };

        DegradedStatus {
            state,
            reads_allowed: reads,
            reconcile_allowed: reconcile,
            creates_allowed: creates,
            deletes_allowed: deletes,
            start_stop_allowed: start_stop,
            last_projection_age_secs: last_age,
        }
    }
}

impl Default for DegradedController {
    fn default() -> Self {
        Self::new_bootstrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_mode_allows_all() {
        let ctrl = DegradedController::new_bootstrap();
        assert_eq!(ctrl.state(), ControlPlaneState::Bootstrap);
        assert!(ctrl.is_allowed(OperationKind::Read));
        assert!(ctrl.is_allowed(OperationKind::Create));
        assert!(ctrl.is_allowed(OperationKind::Delete));
        assert!(ctrl.is_allowed(OperationKind::StartStop));
        assert!(ctrl.is_allowed(OperationKind::Reconcile));
    }

    #[test]
    fn degraded_denies_create_delete() {
        let ctrl = DegradedController {
            bootstrap_mode: AtomicBool::new(false),
            last_projection_at: Mutex::new(None),
            control_plane_reachable: AtomicBool::new(false),
        };
        assert_eq!(ctrl.state(), ControlPlaneState::Degraded);
        assert!(ctrl.is_allowed(OperationKind::Read));
        assert!(ctrl.is_allowed(OperationKind::Reconcile));
        assert!(ctrl.is_allowed(OperationKind::StartStop));
        assert!(!ctrl.is_allowed(OperationKind::Create));
        assert!(!ctrl.is_allowed(OperationKind::Delete));
    }

    #[test]
    fn status_snapshot() {
        let ctrl = DegradedController::new_bootstrap();
        let status = ctrl.status();
        assert_eq!(status.state, ControlPlaneState::Bootstrap);
        assert!(status.creates_allowed);
        assert!(status.deletes_allowed);
    }
}
