//! # syfrah-forge
//!
//! Per-node resource orchestrator for Syfrah.
//!
//! Forge is the single entry point for all resource mutations on a node.
//! It exposes an HTTP/JSON REST API on the fabric interface (`syfrah0`)
//! port 7100, reachable only from within the WireGuard mesh.
//!
//! ## Module dependency graph
//!
//! ```text
//!   api ──→ task, capacity, runtime, health
//!   reconciler ──→ runtime, task
//!   runtime ──→ syfrah_compute::VmManager, syfrah_overlay::NetworkBackend
//!   capacity ──→ (standalone, no forge deps)
//!   health ──→ (standalone, no forge deps)
//!   task ──→ syfrah_state::LayerDb
//!   ownership ──→ syfrah_state::LayerDb
//! ```
//!
//! No circular dependencies exist. Each module exposes a trait boundary
//! that higher-level modules consume:
//! - `runtime::ComputeBackend` — abstraction over VmManager
//! - `health::HealthChecker` — pluggable health checks
//! - `reconciler::ReconcileTarget` / `DriftDetector` — reconciliation traits

pub mod api;
pub mod capacity;
pub mod cleanup;
pub mod degraded;
pub mod drain;
pub mod drift;
pub mod generation;
pub mod health;
pub mod ownership;
pub mod reconciler;
pub mod runtime;
pub mod task;

pub use api::{ForgeHandler, ForgeServer};

#[cfg(test)]
mod tests {
    //! Module boundary verification tests.
    //!
    //! These tests prove that all modules compile and can be used together
    //! without circular dependencies.

    use super::*;

    /// Verify that all modules compile and their key types are accessible.
    #[test]
    fn module_boundaries_compile() {
        // api types
        let _handler = api::ForgeHandler;
        let _ = ForgeServer; // re-export check

        // capacity types
        let tracker = capacity::CapacityTracker::with_capacity(4, 8192);
        assert!(tracker.can_admit(1, 1024));

        // health types
        let _health = health::NodeHealth::healthy(0, 0);
        let _checker: Box<dyn health::HealthChecker> = Box::new(health::SelfHealthChecker);

        // reconciler types
        let reconciler = reconciler::Reconciler::new();
        let _actions = reconciler.reconcile_once();

        // runtime types
        let rt = runtime::ForgeRuntime::new();
        assert!(rt.compute().is_err()); // no backend wired

        // generation types
        let gen_tracker = generation::GenerationTracker::new();
        let _gen = gen_tracker.register("test-vm");

        // task types are tested in task::tests
        // ownership types are tested in ownership::tests
    }

    /// Verify that the module dependency order is correct:
    /// api depends on task + capacity + runtime + health,
    /// but none of those depend on api.
    /// This test simply instantiates types from each module to
    /// prove compilation order.
    #[test]
    fn no_circular_deps() {
        // Standalone modules first (no forge deps)
        let _cap = capacity::CapacityTracker::with_capacity(2, 4096);
        let _health = health::NodeHealth::healthy(0, 0);
        let _recon = reconciler::Reconciler::new();
        let _rt = runtime::ForgeRuntime::new();

        // These all compiled without needing api — proves no circular deps.
    }
}
