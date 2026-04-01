//! Scheduler — filter-then-score placement algorithm.
//!
//! The scheduler runs on the Raft leader. When a `vm create` request arrives:
//!
//! 1. **Filter phase** — eliminate incompatible hypervisors.
//! 2. **Score phase** — rank remaining candidates.
//! 3. Pick the top-scoring hypervisor.
//! 4. Commit `PlaceVm` to Raft with the selected hypervisor.
//!
//! If no gossip data is available (single-node, no gossip running),
//! falls back to "place locally" (current behavior).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::gossip::{GossipCluster, HypervisorGossipReport};

// ---------------------------------------------------------------------------
// Placement constraints (passed from the CLI)
// ---------------------------------------------------------------------------

/// Constraints provided by the user for VM placement.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlacementConstraints {
    /// Zone constraint (e.g. "az-2"). Only hypervisors in this zone are eligible.
    pub zone: Option<String>,
    /// Node selector labels (key=value). All must match.
    pub node_selector: HashMap<String, String>,
    /// Tolerations for taints. Format: "key=value:effect" or "key:effect".
    pub tolerations: Vec<String>,
    /// Anti-affinity group name. VMs in same group prefer different hypervisors.
    pub anti_affinity_group: Option<String>,
    /// Spread topology key (e.g. "zone"). VMs spread across distinct values.
    pub spread_topology: Option<String>,
}

impl PlacementConstraints {
    /// Build constraints from CLI-style arguments.
    pub fn from_cli(
        zone: Option<String>,
        node_selector: &[String],
        anti_affinity: Option<String>,
        spread_topology: Option<String>,
    ) -> Self {
        let mut labels = HashMap::new();
        for sel in node_selector {
            if let Some((key, value)) = sel.split_once('=') {
                labels.insert(key.to_string(), value.to_string());
            }
        }
        Self {
            zone,
            node_selector: labels,
            tolerations: Vec::new(),
            anti_affinity_group: anti_affinity,
            spread_topology,
        }
    }

    /// Summary string for error messages.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref z) = self.zone {
            parts.push(format!("zone={z}"));
        }
        for (k, v) in &self.node_selector {
            parts.push(format!("{k}={v}"));
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        }
    }
}

// ---------------------------------------------------------------------------
// Placement result
// ---------------------------------------------------------------------------

/// The result of a scheduling decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementDecision {
    /// Selected hypervisor node name.
    pub hypervisor_id: String,
    /// Selected hypervisor's fabric IPv6 address.
    pub hypervisor_addr: String,
    /// The score this hypervisor received.
    pub score: f64,
    /// Whether this was a fallback to local placement.
    pub is_local_fallback: bool,
}

/// Error from the scheduler when no hypervisor matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerError {
    pub message: String,
    pub constraints_summary: String,
}

impl std::fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SchedulerError {}

// ---------------------------------------------------------------------------
// Hypervisor info (from gossip + store)
// ---------------------------------------------------------------------------

/// Combined hypervisor info for scheduling decisions.
/// Merges gossip report data with persistent store data.
#[derive(Debug, Clone)]
pub struct HypervisorCandidate {
    pub name: String,
    pub region: String,
    pub zone: String,
    pub state: String,
    pub labels: HashMap<String, String>,
    pub taints: Vec<String>,
    pub allocatable_vcpus: u32,
    pub allocatable_memory_mb: u64,
    pub used_vcpus: u32,
    pub used_memory_mb: u64,
    pub instance_count: u32,
    pub fabric_ipv6: String,
}

impl HypervisorCandidate {
    /// Build from a gossip report.
    pub fn from_gossip_report(report: &HypervisorGossipReport, fabric_ipv6: String) -> Self {
        Self {
            name: report.node_name.clone(),
            region: report.region.clone(),
            zone: report.zone.clone(),
            state: report.state.clone(),
            labels: HashMap::new(),
            taints: Vec::new(),
            allocatable_vcpus: report.allocatable_vcpus,
            allocatable_memory_mb: report.allocatable_memory_mb,
            used_vcpus: report.used_vcpus,
            used_memory_mb: report.used_memory_mb,
            instance_count: report.instance_count,
            fabric_ipv6,
        }
    }

    /// Available vCPUs.
    pub fn available_vcpus(&self) -> u32 {
        self.allocatable_vcpus.saturating_sub(self.used_vcpus)
    }

    /// Available memory MB.
    pub fn available_memory_mb(&self) -> u64 {
        self.allocatable_memory_mb
            .saturating_sub(self.used_memory_mb)
    }

    /// CPU utilization ratio (0.0 – 1.0).
    pub fn cpu_utilization(&self) -> f64 {
        if self.allocatable_vcpus == 0 {
            return 1.0;
        }
        self.used_vcpus as f64 / self.allocatable_vcpus as f64
    }

    /// Memory utilization ratio (0.0 – 1.0).
    pub fn memory_utilization(&self) -> f64 {
        if self.allocatable_memory_mb == 0 {
            return 1.0;
        }
        self.used_memory_mb as f64 / self.allocatable_memory_mb as f64
    }
}

// ---------------------------------------------------------------------------
// Scheduler
// ---------------------------------------------------------------------------

/// The placement scheduler. Runs on the Raft leader.
pub struct Scheduler {
    /// Local node name (for fallback).
    local_node: String,
    /// Local node fabric IPv6 address.
    local_addr: String,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(local_node: String, local_addr: String) -> Self {
        Self {
            local_node,
            local_addr,
        }
    }

    /// Schedule a VM placement.
    ///
    /// - `vcpus` / `memory_mb`: requested resources.
    /// - `constraints`: user-provided placement constraints.
    /// - `cluster`: gossip cluster state (may be empty on single node).
    /// - `excluded`: hypervisors to exclude (e.g. from retry after rejection).
    /// - `existing_placements`: map of hypervisor -> count of VMs in same group (for anti-affinity).
    pub fn schedule(
        &self,
        vcpus: u32,
        memory_mb: u64,
        constraints: &PlacementConstraints,
        cluster: &GossipCluster,
        excluded: &[String],
        existing_placements: &HashMap<String, u32>,
    ) -> Result<PlacementDecision, SchedulerError> {
        let reports = cluster.all_reports();

        // If no gossip data, fall back to local placement.
        if reports.is_empty() {
            info!("scheduler: no gossip data available, falling back to local placement");
            return Ok(PlacementDecision {
                hypervisor_id: self.local_node.clone(),
                hypervisor_addr: self.local_addr.clone(),
                score: 0.0,
                is_local_fallback: true,
            });
        }

        // Build candidates from gossip reports.
        let candidates: Vec<HypervisorCandidate> = reports
            .iter()
            .map(|r| HypervisorCandidate::from_gossip_report(r, r.hypervisor_id.clone()))
            .collect();

        // -- Filter phase --
        let filtered = self.filter(candidates, vcpus, memory_mb, constraints, excluded);

        if filtered.is_empty() {
            let mut parts = Vec::new();
            if let Some(ref z) = constraints.zone {
                parts.push(format!("zone={z}"));
            }
            for (k, v) in &constraints.node_selector {
                parts.push(format!("selector={k}={v}"));
            }
            let summary = if parts.is_empty() {
                "no constraints".to_string()
            } else {
                parts.join(", ")
            };
            return Err(SchedulerError {
                message: format!("no hypervisor matches constraints: {summary}"),
                constraints_summary: summary,
            });
        }

        // -- Score phase --
        let mut scored: Vec<(HypervisorCandidate, f64)> = filtered
            .into_iter()
            .map(|c| {
                let score = self.score(&c, constraints, existing_placements);
                (c, score)
            })
            .collect();

        // Sort by score descending, then by name for determinism.
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.name.cmp(&b.0.name))
        });

        let (winner, score) = scored.into_iter().next().unwrap();
        info!(
            "scheduler: selected hypervisor '{}' (zone={}, score={:.2})",
            winner.name, winner.zone, score
        );

        Ok(PlacementDecision {
            hypervisor_id: winner.name,
            hypervisor_addr: winner.fabric_ipv6,
            score,
            is_local_fallback: false,
        })
    }

    /// Filter phase: eliminate incompatible hypervisors.
    fn filter(
        &self,
        candidates: Vec<HypervisorCandidate>,
        vcpus: u32,
        memory_mb: u64,
        constraints: &PlacementConstraints,
        excluded: &[String],
    ) -> Vec<HypervisorCandidate> {
        candidates
            .into_iter()
            .filter(|c| {
                // State must be Available.
                if c.state != "Available" {
                    debug!("scheduler: filter out '{}' — state={}", c.name, c.state);
                    return false;
                }

                // Not excluded (retry list).
                if excluded.contains(&c.name) {
                    debug!("scheduler: filter out '{}' — excluded", c.name);
                    return false;
                }

                // Zone constraint.
                if let Some(ref zone) = constraints.zone {
                    if c.zone != *zone {
                        debug!(
                            "scheduler: filter out '{}' — zone mismatch (want={}, have={})",
                            c.name, zone, c.zone
                        );
                        return false;
                    }
                }

                // Node selector labels.
                for (key, value) in &constraints.node_selector {
                    match c.labels.get(key) {
                        Some(v) if v == value => {}
                        _ => {
                            debug!(
                                "scheduler: filter out '{}' — label mismatch {}={}",
                                c.name, key, value
                            );
                            return false;
                        }
                    }
                }

                // Taints — skip if hypervisor has NoSchedule taint not tolerated.
                for taint in &c.taints {
                    if taint.contains("NoSchedule") && !constraints.tolerations.contains(taint) {
                        debug!(
                            "scheduler: filter out '{}' — taint not tolerated: {}",
                            c.name, taint
                        );
                        return false;
                    }
                }

                // Capacity check.
                if c.available_vcpus() < vcpus {
                    debug!(
                        "scheduler: filter out '{}' — insufficient vCPUs (need={}, avail={})",
                        c.name,
                        vcpus,
                        c.available_vcpus()
                    );
                    return false;
                }
                if c.available_memory_mb() < memory_mb {
                    debug!(
                        "scheduler: filter out '{}' — insufficient memory (need={}, avail={})",
                        c.name,
                        memory_mb,
                        c.available_memory_mb()
                    );
                    return false;
                }

                true
            })
            .collect()
    }

    /// Score phase: rank candidates. Higher is better.
    fn score(
        &self,
        candidate: &HypervisorCandidate,
        constraints: &PlacementConstraints,
        existing_placements: &HashMap<String, u32>,
    ) -> f64 {
        let mut score = 0.0;

        // Lower utilization → higher score (100 points max for an empty node).
        let avg_util = (candidate.cpu_utilization() + candidate.memory_utilization()) / 2.0;
        score += (1.0 - avg_util) * 100.0;

        // Spread bonus: if spread_topology is set, prefer hypervisors with
        // fewer VMs in the same group.
        if constraints.spread_topology.is_some() || constraints.anti_affinity_group.is_some() {
            let count = existing_placements
                .get(&candidate.name)
                .copied()
                .unwrap_or(0);
            // Penalty: -20 per existing VM on this hypervisor.
            score -= count as f64 * 20.0;
        }

        score
    }

    /// Schedule with retry: pick a hypervisor, let the caller perform an
    /// admission recheck on the target Forge. If rejected, retry with
    /// the rejected hypervisor excluded. Up to `max_retries` attempts.
    ///
    /// The `admission_check` closure receives a `PlacementDecision` and
    /// returns `Ok(())` if admitted, or `Err(reason)` if rejected.
    pub fn schedule_with_retry<F>(
        &self,
        vcpus: u32,
        memory_mb: u64,
        constraints: &PlacementConstraints,
        cluster: &GossipCluster,
        existing_placements: &HashMap<String, u32>,
        max_retries: usize,
        mut admission_check: F,
    ) -> Result<PlacementDecision, SchedulerError>
    where
        F: FnMut(&PlacementDecision) -> Result<(), String>,
    {
        let mut excluded: Vec<String> = Vec::new();

        for attempt in 0..=max_retries {
            let decision = self.schedule(
                vcpus,
                memory_mb,
                constraints,
                cluster,
                &excluded,
                existing_placements,
            )?;

            // Local fallback skips admission recheck.
            if decision.is_local_fallback {
                return Ok(decision);
            }

            match admission_check(&decision) {
                Ok(()) => {
                    info!(
                        "scheduler: admission accepted on attempt {} for '{}'",
                        attempt + 1,
                        decision.hypervisor_id
                    );
                    return Ok(decision);
                }
                Err(reason) => {
                    warn!(
                        "scheduler: admission rejected on '{}' (attempt {}): {}",
                        decision.hypervisor_id,
                        attempt + 1,
                        reason
                    );
                    excluded.push(decision.hypervisor_id.clone());
                }
            }
        }

        Err(SchedulerError {
            message: format!(
                "all {} hypervisors rejected admission after {} retries",
                excluded.len(),
                max_retries + 1
            ),
            constraints_summary: constraints.summary(),
        })
    }
}

/// Maximum number of admission recheck retries.
pub const MAX_ADMISSION_RETRIES: usize = 3;

/// Result of a Forge admission recheck.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AdmissionResult {
    /// Admission accepted — proceed with VM creation.
    Accepted,
    /// Admission rejected — stale data, capacity changed.
    Rejected { reason: String },
}

impl AdmissionResult {
    /// Check if admission was accepted.
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gossip::{GossipCluster, HypervisorGossipReport};

    fn make_report(
        name: &str,
        zone: &str,
        alloc_vcpus: u32,
        used_vcpus: u32,
        alloc_mem: u64,
        used_mem: u64,
    ) -> HypervisorGossipReport {
        HypervisorGossipReport {
            hypervisor_id: format!("{name}-id"),
            node_name: name.to_string(),
            region: "eu-west".to_string(),
            zone: zone.to_string(),
            state: "Available".to_string(),
            allocatable_vcpus: alloc_vcpus,
            allocatable_memory_mb: alloc_mem,
            used_vcpus,
            used_memory_mb: used_mem,
            instance_count: 1,
            drain_status: false,
            timestamp: 1000,
        }
    }

    #[test]
    fn fallback_when_no_gossip_data() {
        let cluster = GossipCluster::new();
        let scheduler = Scheduler::new("hv-local".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        let result = scheduler
            .schedule(2, 4096, &constraints, &cluster, &[], &HashMap::new())
            .unwrap();
        assert!(result.is_local_fallback);
        assert_eq!(result.hypervisor_id, "hv-local");
    }

    #[test]
    fn picks_least_loaded_hypervisor() {
        let cluster = GossipCluster::new();
        // hv-1: 50% used, hv-2: 25% used
        cluster.update_report(make_report("hv-1", "az-1", 8, 4, 16384, 8192));
        cluster.update_report(make_report("hv-2", "az-2", 8, 2, 16384, 4096));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        let result = scheduler
            .schedule(1, 1024, &constraints, &cluster, &[], &HashMap::new())
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
        assert!(!result.is_local_fallback);
    }

    #[test]
    fn zone_filter() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1", 8, 0, 16384, 0));
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints {
            zone: Some("az-2".to_string()),
            ..Default::default()
        };

        let result = scheduler
            .schedule(1, 1024, &constraints, &cluster, &[], &HashMap::new())
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
    }

    #[test]
    fn zone_filter_no_match_returns_error() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints {
            zone: Some("az-99".to_string()),
            ..Default::default()
        };

        let err = scheduler
            .schedule(1, 1024, &constraints, &cluster, &[], &HashMap::new())
            .unwrap_err();
        assert!(err.message.contains("no hypervisor matches constraints"));
        assert!(err.constraints_summary.contains("zone=az-99"));
    }

    #[test]
    fn capacity_filter() {
        let cluster = GossipCluster::new();
        // hv-1 has only 1 vCPU free
        cluster.update_report(make_report("hv-1", "az-1", 8, 7, 16384, 0));
        // hv-2 has plenty
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        let result = scheduler
            .schedule(4, 4096, &constraints, &cluster, &[], &HashMap::new())
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
    }

    #[test]
    fn excluded_hypervisors_skipped() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1", 8, 0, 16384, 0));
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        let result = scheduler
            .schedule(
                1,
                1024,
                &constraints,
                &cluster,
                &["hv-1".to_string()],
                &HashMap::new(),
            )
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
    }

    #[test]
    fn anti_affinity_penalizes_colocated() {
        let cluster = GossipCluster::new();
        // Both equally loaded
        cluster.update_report(make_report("hv-1", "az-1", 8, 2, 16384, 4096));
        cluster.update_report(make_report("hv-2", "az-2", 8, 2, 16384, 4096));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints {
            anti_affinity_group: Some("web".to_string()),
            ..Default::default()
        };

        // hv-1 already has 3 VMs from the same group
        let mut existing = HashMap::new();
        existing.insert("hv-1".to_string(), 3);

        let result = scheduler
            .schedule(1, 1024, &constraints, &cluster, &[], &existing)
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
    }

    #[test]
    fn state_not_available_filtered() {
        let cluster = GossipCluster::new();
        let mut report = make_report("hv-1", "az-1", 8, 0, 16384, 0);
        report.state = "Draining".to_string();
        cluster.update_report(report);
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        let result = scheduler
            .schedule(1, 1024, &constraints, &cluster, &[], &HashMap::new())
            .unwrap();
        assert_eq!(result.hypervisor_id, "hv-2");
    }

    #[test]
    fn placement_constraints_default() {
        let c = PlacementConstraints::default();
        assert!(c.zone.is_none());
        assert!(c.node_selector.is_empty());
        assert!(c.tolerations.is_empty());
        assert!(c.anti_affinity_group.is_none());
        assert!(c.spread_topology.is_none());
    }

    #[test]
    fn hypervisor_candidate_utilization() {
        let c = HypervisorCandidate {
            name: "hv".to_string(),
            region: "eu".to_string(),
            zone: "az-1".to_string(),
            state: "Available".to_string(),
            labels: HashMap::new(),
            taints: Vec::new(),
            allocatable_vcpus: 10,
            allocatable_memory_mb: 20480,
            used_vcpus: 5,
            used_memory_mb: 10240,
            instance_count: 3,
            fabric_ipv6: "::1".to_string(),
        };
        assert!((c.cpu_utilization() - 0.5).abs() < f64::EPSILON);
        assert!((c.memory_utilization() - 0.5).abs() < f64::EPSILON);
        assert_eq!(c.available_vcpus(), 5);
        assert_eq!(c.available_memory_mb(), 10240);
    }

    #[test]
    fn retry_on_first_rejection_picks_second() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1", 8, 0, 16384, 0));
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();
        let mut call_count = 0;

        let result = scheduler
            .schedule_with_retry(
                1,
                1024,
                &constraints,
                &cluster,
                &HashMap::new(),
                3,
                |decision| {
                    call_count += 1;
                    if call_count == 1 {
                        // Reject the first pick
                        Err("capacity stale".to_string())
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap();

        assert_eq!(call_count, 2);
        // The second hypervisor should have been picked
        assert!(!result.is_local_fallback);
    }

    #[test]
    fn retry_exhaustion_fails() {
        let cluster = GossipCluster::new();
        cluster.update_report(make_report("hv-1", "az-1", 8, 0, 16384, 0));
        cluster.update_report(make_report("hv-2", "az-2", 8, 0, 16384, 0));

        let scheduler = Scheduler::new("hv-1".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();

        // All admission checks fail
        let result = scheduler.schedule_with_retry(
            1,
            1024,
            &constraints,
            &cluster,
            &HashMap::new(),
            1, // max 1 retry = 2 attempts total (but only 2 hypervisors)
            |_| Err("always reject".to_string()),
        );

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("rejected admission"));
    }

    #[test]
    fn retry_local_fallback_skips_check() {
        let cluster = GossipCluster::new(); // empty = fallback

        let scheduler = Scheduler::new("hv-local".to_string(), "::1".to_string());
        let constraints = PlacementConstraints::default();
        let mut called = false;

        let result = scheduler
            .schedule_with_retry(1, 1024, &constraints, &cluster, &HashMap::new(), 3, |_| {
                called = true;
                Err("should not be called".to_string())
            })
            .unwrap();

        assert!(!called);
        assert!(result.is_local_fallback);
    }

    #[test]
    fn admission_result_accepted() {
        let r = AdmissionResult::Accepted;
        assert!(r.is_accepted());
        let r2 = AdmissionResult::Rejected {
            reason: "full".to_string(),
        };
        assert!(!r2.is_accepted());
    }

    #[test]
    fn from_cli_parses_node_selector() {
        let c = PlacementConstraints::from_cli(
            Some("az-2".to_string()),
            &["gpu=a100".to_string(), "tier=premium".to_string()],
            None,
            None,
        );
        assert_eq!(c.zone, Some("az-2".to_string()));
        assert_eq!(c.node_selector.get("gpu"), Some(&"a100".to_string()));
        assert_eq!(c.node_selector.get("tier"), Some(&"premium".to_string()));
    }
}
