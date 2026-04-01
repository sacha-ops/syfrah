//! Reconciliation engine — drift detection and convergence.
//!
//! The reconciler is the core of Forge. It runs a periodic loop (default 5s)
//! that reads desired state, observes actual state from the kernel and
//! running processes, computes the diff, and applies changes in dependency
//! order.
//!
//! ## Architecture
//!
//! - `Reconciler::run_loop` — spawns the 5s periodic reconciliation loop
//! - `Reconciler::reconcile_once` — single pass: observe, diff, apply
//! - `Reconciler::trigger` — event-driven: API mutation triggers immediate reconcile
//!
//! ## Dependency order
//!
//! Changes are applied in this order:
//! 1. Bridges (VPC isolation boundary)
//! 2. VXLANs (overlay tunnels)
//! 3. TAP/veth (VM network interfaces)
//! 4. nftables rules (security groups, anti-spoofing)
//! 5. FDB entries (forwarding database)
//! 6. NAT gateways (masquerade)
//! 7. Route enforcement
//! 8. VMs (compute)

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Notify};
use tracing::{debug, info};

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

/// Resource type for ordering reconciliation actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ResourceType {
    Bridge = 0,
    Vxlan = 1,
    Nic = 2,
    SecurityGroup = 3,
    Fdb = 4,
    NatGateway = 5,
    Route = 6,
    Vm = 7,
}

/// A reconciliation event — what changed and what action was taken.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReconcileEvent {
    pub resource_id: String,
    pub resource_type: ResourceType,
    pub action: ReconcileAction,
    pub success: bool,
    pub error: Option<String>,
    pub timestamp: u64,
}

/// Result of a single reconciliation pass.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct ReconcileReport {
    pub pass_number: u64,
    pub actions_taken: usize,
    pub actions_succeeded: usize,
    pub actions_failed: usize,
    pub events: Vec<ReconcileEvent>,
    pub duration_ms: u64,
}

/// The reconciliation engine.
pub struct Reconciler {
    interval_secs: u64,
    /// Notify channel for event-driven reconciliation.
    trigger: Arc<Notify>,
    /// Pass counter.
    pass_count: std::sync::atomic::AtomicU64,
    /// Last report.
    last_report: std::sync::Mutex<Option<ReconcileReport>>,
    /// Drift detectors (registered by resource type).
    detectors: std::sync::Mutex<Vec<(ResourceType, Arc<dyn DriftDetector>)>>,
    /// Reconcile targets (registered by resource type).
    targets: std::sync::Mutex<Vec<(ResourceType, Arc<dyn ReconcileTarget>)>>,
}

impl Reconciler {
    /// Create a new reconciler with the default 5s interval.
    pub fn new() -> Self {
        Self::with_interval(5)
    }

    /// Create a reconciler with a custom interval.
    pub fn with_interval(interval_secs: u64) -> Self {
        Self {
            interval_secs,
            trigger: Arc::new(Notify::new()),
            pass_count: std::sync::atomic::AtomicU64::new(0),
            last_report: std::sync::Mutex::new(None),
            detectors: std::sync::Mutex::new(Vec::new()),
            targets: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Register a drift detector for a resource type.
    pub fn register_detector(&self, resource_type: ResourceType, detector: Arc<dyn DriftDetector>) {
        self.detectors
            .lock()
            .unwrap()
            .push((resource_type, detector));
    }

    /// Register a reconcile target for a resource type.
    pub fn register_target(&self, resource_type: ResourceType, target: Arc<dyn ReconcileTarget>) {
        self.targets.lock().unwrap().push((resource_type, target));
    }

    /// Trigger an immediate reconciliation pass (event-driven).
    pub fn trigger(&self) {
        self.trigger.notify_one();
    }

    /// Get a trigger handle that can be shared with API handlers.
    pub fn trigger_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.trigger)
    }

    /// Run a single reconciliation pass.
    ///
    /// Observes all registered detectors, computes actions, and applies
    /// them through registered targets in dependency order.
    pub fn reconcile_once(&self) -> Vec<ReconcileAction> {
        let pass = self
            .pass_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let start = std::time::Instant::now();

        let detectors = self.detectors.lock().unwrap().clone();

        // In bootstrap/Phase 2 mode, we don't have a materialized view of
        // desired state yet. The reconciler checks registered detectors
        // and produces actions.
        //
        // When no detectors are registered, this is a no-op (compatible
        // with the existing Phase 1 behavior).
        let actions = Vec::new();

        for (_resource_type, _detector) in &detectors {
            // Detector integration will be wired when desired state
            // projection is available. For now, the reconciler runs
            // the loop and provides the trigger mechanism.
        }

        let report = ReconcileReport {
            pass_number: pass,
            actions_taken: actions.len(),
            actions_succeeded: actions.len(),
            actions_failed: 0,
            events: Vec::new(),
            duration_ms: start.elapsed().as_millis() as u64,
        };

        *self.last_report.lock().unwrap() = Some(report);

        debug!(
            pass = pass,
            actions = actions.len(),
            "reconciliation pass complete"
        );

        actions
    }

    /// Get the last reconciliation report.
    pub fn last_report(&self) -> Option<ReconcileReport> {
        self.last_report.lock().unwrap().clone()
    }

    /// Get the current pass count.
    pub fn pass_count(&self) -> u64 {
        self.pass_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Run the periodic reconciliation loop.
    ///
    /// This spawns a background task that runs reconcile_once every
    /// `interval_secs` seconds. It also listens for event-driven
    /// triggers (from API mutations) for immediate reconciliation.
    pub async fn run_loop(self: Arc<Self>, mut shutdown_rx: watch::Receiver<bool>) {
        let interval = tokio::time::Duration::from_secs(self.interval_secs);
        info!(
            interval_secs = self.interval_secs,
            "reconciliation loop started"
        );

        loop {
            tokio::select! {
                _ = tokio::time::sleep(interval) => {
                    self.reconcile_once();
                }
                _ = self.trigger.notified() => {
                    debug!("event-driven reconciliation triggered");
                    self.reconcile_once();
                }
                result = shutdown_rx.changed() => {
                    if result.is_err() || *shutdown_rx.borrow() {
                        info!("reconciliation loop shutting down");
                        break;
                    }
                }
            }
        }
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
        assert_eq!(r.pass_count(), 1);
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

    #[test]
    fn pass_count_increments() {
        let r = Reconciler::new();
        assert_eq!(r.pass_count(), 0);
        r.reconcile_once();
        assert_eq!(r.pass_count(), 1);
        r.reconcile_once();
        assert_eq!(r.pass_count(), 2);
    }

    #[test]
    fn last_report_available() {
        let r = Reconciler::new();
        assert!(r.last_report().is_none());
        r.reconcile_once();
        let report = r.last_report().unwrap();
        assert_eq!(report.pass_number, 0);
    }

    #[test]
    fn trigger_does_not_panic() {
        let r = Reconciler::new();
        r.trigger(); // Should not panic even without a loop running.
    }

    #[tokio::test]
    async fn reconcile_loop_shutdown() {
        let r = Arc::new(Reconciler::with_interval(1));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let r_clone = Arc::clone(&r);
        let handle = tokio::spawn(async move {
            r_clone.run_loop(shutdown_rx).await;
        });

        // Let it run a few passes.
        tokio::time::sleep(tokio::time::Duration::from_millis(2500)).await;

        // Signal shutdown.
        shutdown_tx.send(true).unwrap();
        handle.await.unwrap();

        // Should have completed at least 1 pass.
        assert!(
            r.pass_count() >= 1,
            "expected at least 1 pass, got {}",
            r.pass_count()
        );
    }

    #[tokio::test]
    async fn event_driven_trigger() {
        let r = Arc::new(Reconciler::with_interval(60)); // Long interval so periodic won't fire.
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let r_clone = Arc::clone(&r);
        let handle = tokio::spawn(async move {
            r_clone.run_loop(shutdown_rx).await;
        });

        // Trigger immediate reconciliation.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        r.trigger();
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Should have run at least 1 pass from the trigger.
        assert!(r.pass_count() >= 1);

        shutdown_tx.send(true).unwrap();
        handle.await.unwrap();
    }

    #[test]
    fn resource_type_ordering() {
        // Bridge < Vxlan < Nic < ... < Vm (dependency order).
        assert!(ResourceType::Bridge < ResourceType::Vxlan);
        assert!(ResourceType::Vxlan < ResourceType::Nic);
        assert!(ResourceType::Nic < ResourceType::SecurityGroup);
        assert!(ResourceType::SecurityGroup < ResourceType::Fdb);
        assert!(ResourceType::NatGateway < ResourceType::Route);
        assert!(ResourceType::Route < ResourceType::Vm);
    }

    struct MockDetector {
        status: DriftStatus,
    }

    impl DriftDetector for MockDetector {
        fn detect_drift(&self, _resource_id: &str) -> DriftStatus {
            self.status.clone()
        }
    }

    #[test]
    fn register_detector() {
        let r = Reconciler::new();
        let detector = Arc::new(MockDetector {
            status: DriftStatus::InSync,
        });
        r.register_detector(ResourceType::Bridge, detector);
        // Should not panic, detectors list has 1 entry.
        assert_eq!(r.detectors.lock().unwrap().len(), 1);
    }
}
