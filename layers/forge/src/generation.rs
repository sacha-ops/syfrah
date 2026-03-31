//! Generation tracking for resource drift detection.
//!
//! Every resource managed by Forge gets three generation fields:
//! - `spec_generation` (u64): incremented on every desired-state change
//! - `reconcile_generation` (u64): updated to match spec_generation after reconciliation
//! - `last_observed_at` (u64 unix timestamp): when the resource was last observed
//!
//! Drift detection: `spec_generation != reconcile_generation`
//! Staleness detection: `now - last_observed_at > threshold`

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Default staleness threshold in seconds (5 minutes).
const DEFAULT_STALENESS_THRESHOLD_SECS: u64 = 300;

/// Generation metadata for a single resource.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ResourceGeneration {
    /// Resource identifier.
    pub resource_id: String,
    /// Current desired-state generation. Incremented on every spec change.
    pub spec_generation: u64,
    /// Last reconciled generation. Updated after successful reconciliation.
    pub reconcile_generation: u64,
    /// Unix timestamp of last observation.
    pub last_observed_at: u64,
}

impl ResourceGeneration {
    /// Create a new resource generation tracker.
    pub fn new(resource_id: &str) -> Self {
        Self {
            resource_id: resource_id.to_string(),
            spec_generation: 1,
            reconcile_generation: 0,
            last_observed_at: now_unix(),
        }
    }

    /// Check if the resource has drifted (spec != reconcile).
    pub fn has_drift(&self) -> bool {
        self.spec_generation != self.reconcile_generation
    }

    /// Check if the resource is stale (not observed recently).
    pub fn is_stale(&self, threshold_secs: u64) -> bool {
        let now = now_unix();
        now.saturating_sub(self.last_observed_at) > threshold_secs
    }

    /// Increment the spec generation (desired state changed).
    pub fn bump_spec(&mut self) {
        self.spec_generation += 1;
        self.last_observed_at = now_unix();
    }

    /// Mark reconciliation complete (reconcile catches up to spec).
    pub fn mark_reconciled(&mut self) {
        self.reconcile_generation = self.spec_generation;
        self.last_observed_at = now_unix();
    }

    /// Update the last observed timestamp.
    pub fn touch(&mut self) {
        self.last_observed_at = now_unix();
    }
}

/// In-memory generation tracker for all resources.
pub struct GenerationTracker {
    generations: Mutex<HashMap<String, ResourceGeneration>>,
    staleness_threshold_secs: u64,
}

impl GenerationTracker {
    /// Create a new generation tracker with the default staleness threshold.
    pub fn new() -> Self {
        Self {
            generations: Mutex::new(HashMap::new()),
            staleness_threshold_secs: DEFAULT_STALENESS_THRESHOLD_SECS,
        }
    }

    /// Create with a custom staleness threshold (for testing).
    pub fn with_threshold(threshold_secs: u64) -> Self {
        Self {
            generations: Mutex::new(HashMap::new()),
            staleness_threshold_secs: threshold_secs,
        }
    }

    /// Register a new resource. Returns the initial generation.
    pub fn register(&self, resource_id: &str) -> ResourceGeneration {
        let gen = ResourceGeneration::new(resource_id);
        let mut map = self.generations.lock().unwrap();
        map.insert(resource_id.to_string(), gen.clone());
        gen
    }

    /// Increment the spec generation for a resource.
    pub fn bump_spec(&self, resource_id: &str) -> Option<ResourceGeneration> {
        let mut map = self.generations.lock().unwrap();
        if let Some(gen) = map.get_mut(resource_id) {
            gen.bump_spec();
            Some(gen.clone())
        } else {
            None
        }
    }

    /// Mark a resource as reconciled.
    pub fn mark_reconciled(&self, resource_id: &str) -> Option<ResourceGeneration> {
        let mut map = self.generations.lock().unwrap();
        if let Some(gen) = map.get_mut(resource_id) {
            gen.mark_reconciled();
            Some(gen.clone())
        } else {
            None
        }
    }

    /// Touch a resource (update last_observed_at).
    pub fn touch(&self, resource_id: &str) -> Option<ResourceGeneration> {
        let mut map = self.generations.lock().unwrap();
        if let Some(gen) = map.get_mut(resource_id) {
            gen.touch();
            Some(gen.clone())
        } else {
            None
        }
    }

    /// Get the generation for a resource.
    pub fn get(&self, resource_id: &str) -> Option<ResourceGeneration> {
        let map = self.generations.lock().unwrap();
        map.get(resource_id).cloned()
    }

    /// Remove a resource from tracking.
    pub fn remove(&self, resource_id: &str) -> Option<ResourceGeneration> {
        let mut map = self.generations.lock().unwrap();
        map.remove(resource_id)
    }

    /// List all resources with drift (spec != reconcile).
    pub fn drifted(&self) -> Vec<ResourceGeneration> {
        let map = self.generations.lock().unwrap();
        map.values().filter(|g| g.has_drift()).cloned().collect()
    }

    /// List all stale resources.
    pub fn stale(&self) -> Vec<ResourceGeneration> {
        let map = self.generations.lock().unwrap();
        map.values()
            .filter(|g| g.is_stale(self.staleness_threshold_secs))
            .cloned()
            .collect()
    }

    /// List all tracked resources.
    pub fn all(&self) -> Vec<ResourceGeneration> {
        let map = self.generations.lock().unwrap();
        map.values().cloned().collect()
    }
}

impl Default for GenerationTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_generation_increments() {
        let tracker = GenerationTracker::new();
        let gen = tracker.register("vm-1");
        assert_eq!(gen.spec_generation, 1);
        assert_eq!(gen.reconcile_generation, 0);

        let gen2 = tracker.bump_spec("vm-1").unwrap();
        assert_eq!(gen2.spec_generation, 2);
        assert_eq!(gen2.reconcile_generation, 0);

        let gen3 = tracker.bump_spec("vm-1").unwrap();
        assert_eq!(gen3.spec_generation, 3);
    }

    #[test]
    fn reconcile_generation_updated() {
        let tracker = GenerationTracker::new();
        tracker.register("vm-1");
        tracker.bump_spec("vm-1");

        let gen = tracker.mark_reconciled("vm-1").unwrap();
        assert_eq!(gen.spec_generation, 2);
        assert_eq!(gen.reconcile_generation, 2);
        assert!(!gen.has_drift());
    }

    #[test]
    fn drift_detected() {
        let tracker = GenerationTracker::new();
        tracker.register("vm-1");

        // New resource has drift (spec=1, reconcile=0).
        let gen = tracker.get("vm-1").unwrap();
        assert!(gen.has_drift());

        // Reconcile clears drift.
        tracker.mark_reconciled("vm-1");
        let gen = tracker.get("vm-1").unwrap();
        assert!(!gen.has_drift());

        // Bump spec creates drift again.
        tracker.bump_spec("vm-1");
        let gen = tracker.get("vm-1").unwrap();
        assert!(gen.has_drift());

        // Drifted list should contain this resource.
        let drifted = tracker.drifted();
        assert_eq!(drifted.len(), 1);
        assert_eq!(drifted[0].resource_id, "vm-1");
    }

    #[test]
    fn staleness_detected() {
        // Use a zero threshold so everything is immediately stale.
        let tracker = GenerationTracker::with_threshold(0);
        tracker.register("vm-1");

        // With threshold=0, resource should be stale (last_observed_at == now,
        // but now - now = 0 which is NOT > 0). So actually not stale.
        let gen = tracker.get("vm-1").unwrap();
        assert!(!gen.is_stale(0)); // 0 - 0 is not > 0

        // With threshold=0 and a modified timestamp, it would be stale.
        let mut gen2 = gen.clone();
        gen2.last_observed_at = now_unix().saturating_sub(10);
        assert!(gen2.is_stale(5)); // 10 > 5 → stale
    }

    #[test]
    fn remove_cleans_up() {
        let tracker = GenerationTracker::new();
        tracker.register("vm-1");
        assert!(tracker.get("vm-1").is_some());

        tracker.remove("vm-1");
        assert!(tracker.get("vm-1").is_none());
    }

    #[test]
    fn all_lists_tracked_resources() {
        let tracker = GenerationTracker::new();
        tracker.register("vm-1");
        tracker.register("vm-2");
        tracker.register("vm-3");

        let all = tracker.all();
        assert_eq!(all.len(), 3);
    }
}
