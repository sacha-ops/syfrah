//! Reconciliation engine — drift detection and convergence.
//!
//! In Phase 1 (bootstrap mode), the reconciler is a stub that provides
//! the trait interface for future phases.
//!
//! ## Trait boundaries
//!
//! The `ReconcileTarget` trait defines what the reconciler can act on.
//! The `DriftDetector` trait detects divergence between desired and actual state.
//! Both are designed to be implemented by higher-level components.

use serde::{Deserialize, Serialize};

/// Drift detection result for a single resource.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DriftStatus {
    /// Resource matches desired state.
    InSync,
    /// Resource diverges from desired state.
    Drifted { reason: String },
    /// Resource exists but has no desired state (orphan).
    Orphaned,
    /// Desired state exists but resource is missing.
    Missing,
}

/// A reconciliation action to take.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub enum ReconcileAction {
    Create { resource_id: String },
    Update { resource_id: String, reason: String },
    Delete { resource_id: String },
    NoOp { resource_id: String },
}

/// Trait for components that can detect drift.
pub trait DriftDetector: Send + Sync {
    /// Check if a resource has drifted from desired state.
    fn detect_drift(&self, resource_id: &str) -> DriftStatus;
}

/// Trait for targets that the reconciler can converge.
#[async_trait::async_trait]
pub trait ReconcileTarget: Send + Sync {
    /// Apply a reconciliation action.
    async fn apply(&self, action: &ReconcileAction) -> Result<(), String>;
}

/// Placeholder for the reconciliation engine.
pub struct Reconciler {
    _interval_secs: u64,
}

impl Reconciler {
    /// Create a new reconciler (no-op in bootstrap mode).
    pub fn new() -> Self {
        Self { _interval_secs: 30 }
    }

    /// Create a reconciler with a custom interval.
    pub fn with_interval(interval_secs: u64) -> Self {
        Self {
            _interval_secs: interval_secs,
        }
    }

    /// Run a single reconciliation pass (stub in Phase 1).
    pub fn reconcile_once(&self) -> Vec<ReconcileAction> {
        // In bootstrap mode, no reconciliation is performed.
        // The reconciler will be wired to the materialized view in Phase 2.
        Vec::new()
    }
}

impl Default for Reconciler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciler_creates() {
        let r = Reconciler::new();
        assert!(r.reconcile_once().is_empty());
    }

    #[test]
    fn reconciler_with_interval() {
        let r = Reconciler::with_interval(60);
        assert!(r.reconcile_once().is_empty());
    }

    #[test]
    fn drift_status_variants() {
        let in_sync = DriftStatus::InSync;
        let drifted = DriftStatus::Drifted {
            reason: "config changed".into(),
        };
        let orphaned = DriftStatus::Orphaned;
        let missing = DriftStatus::Missing;

        assert_eq!(in_sync, DriftStatus::InSync);
        assert_ne!(drifted, DriftStatus::InSync);
        assert_ne!(orphaned, missing);
    }

    #[test]
    fn reconcile_action_serializes() {
        let action = ReconcileAction::Create {
            resource_id: "vm-1".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("vm-1"));
    }
}
