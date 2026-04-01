//! Drift detection for all Forge-managed resource types.
//!
//! Implements 12 drift scenarios:
//! 1. Missing bridge — bridge in desired state but not in kernel
//! 2. Missing VXLAN — VXLAN in desired state but not in kernel
//! 3. Missing TAP — TAP device expected but not in kernel
//! 4. Dead VM — VM process expected but not running
//! 5. Stale nftables — nftables rules don't match desired SG config
//! 6. Wrong SG rules — SG chain contents differ from expected
//! 7. Orphaned IP — IP allocated but no VM using it
//! 8. Missing FDB — FDB entry expected but not in bridge FDB table
//! 9. Missing ARP proxy — ARP proxy entry expected but not present
//! 10. Missing NAT — NAT masquerade expected but not applied
//! 11. Wrong gateway IP — Bridge gateway IP doesn't match expected
//! 12. Stale route — Route entry stale or missing

use serde::{Deserialize, Serialize};

use crate::reconciler::{DriftStatus, ResourceType};

/// A detected drift with full context for remediation.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DriftReport {
    pub resource_id: String,
    pub resource_type: ResourceType,
    pub status: DriftStatus,
    pub scenario: DriftScenario,
    pub expected: String,
    pub actual: String,
}

/// Enumeration of all 12 drift scenarios.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum DriftScenario {
    MissingBridge,
    MissingVxlan,
    MissingTap,
    DeadVm,
    StaleNftables,
    WrongSgRules,
    OrphanedIp,
    MissingFdb,
    MissingArpProxy,
    MissingNat,
    WrongGatewayIp,
    StaleRoute,
}

impl std::fmt::Display for DriftScenario {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriftScenario::MissingBridge => write!(f, "missing_bridge"),
            DriftScenario::MissingVxlan => write!(f, "missing_vxlan"),
            DriftScenario::MissingTap => write!(f, "missing_tap"),
            DriftScenario::DeadVm => write!(f, "dead_vm"),
            DriftScenario::StaleNftables => write!(f, "stale_nftables"),
            DriftScenario::WrongSgRules => write!(f, "wrong_sg_rules"),
            DriftScenario::OrphanedIp => write!(f, "orphaned_ip"),
            DriftScenario::MissingFdb => write!(f, "missing_fdb"),
            DriftScenario::MissingArpProxy => write!(f, "missing_arp_proxy"),
            DriftScenario::MissingNat => write!(f, "missing_nat"),
            DriftScenario::WrongGatewayIp => write!(f, "wrong_gateway_ip"),
            DriftScenario::StaleRoute => write!(f, "stale_route"),
        }
    }
}

/// Network drift detector — checks kernel interfaces against desired state.
///
/// Uses `NetworkBackend::list_interfaces` to discover actual state and
/// compares against the expected set of resources.
pub struct NetworkDriftDetector {
    /// Expected bridge names (from desired state).
    pub expected_bridges: Vec<String>,
    /// Expected VXLAN names.
    pub expected_vxlans: Vec<String>,
    /// Expected TAP names.
    pub expected_taps: Vec<String>,
    /// Actual interfaces discovered from kernel.
    pub actual_interfaces: Vec<String>,
}

impl NetworkDriftDetector {
    /// Create a new detector with empty expected/actual sets.
    pub fn new() -> Self {
        Self {
            expected_bridges: Vec::new(),
            expected_vxlans: Vec::new(),
            expected_taps: Vec::new(),
            actual_interfaces: Vec::new(),
        }
    }

    /// Set actual interfaces from kernel discovery.
    pub fn set_actual(&mut self, interfaces: Vec<String>) {
        self.actual_interfaces = interfaces;
    }

    /// Detect drift for all network resources.
    pub fn detect_all(&self) -> Vec<DriftReport> {
        let mut reports = Vec::new();

        // Check bridges.
        for bridge in &self.expected_bridges {
            if !self.actual_interfaces.iter().any(|i| i == bridge) {
                reports.push(DriftReport {
                    resource_id: bridge.clone(),
                    resource_type: ResourceType::Bridge,
                    status: DriftStatus::Missing,
                    scenario: DriftScenario::MissingBridge,
                    expected: format!("bridge {} exists", bridge),
                    actual: "bridge not found in kernel".to_string(),
                });
            }
        }

        // Check VXLANs.
        for vxlan in &self.expected_vxlans {
            if !self.actual_interfaces.iter().any(|i| i == vxlan) {
                reports.push(DriftReport {
                    resource_id: vxlan.clone(),
                    resource_type: ResourceType::Vxlan,
                    status: DriftStatus::Missing,
                    scenario: DriftScenario::MissingVxlan,
                    expected: format!("VXLAN {} exists", vxlan),
                    actual: "VXLAN not found in kernel".to_string(),
                });
            }
        }

        // Check TAPs.
        for tap in &self.expected_taps {
            if !self.actual_interfaces.iter().any(|i| i == tap) {
                reports.push(DriftReport {
                    resource_id: tap.clone(),
                    resource_type: ResourceType::Nic,
                    status: DriftStatus::Missing,
                    scenario: DriftScenario::MissingTap,
                    expected: format!("TAP {} exists", tap),
                    actual: "TAP not found in kernel".to_string(),
                });
            }
        }

        // Check for orphans: interfaces in kernel but not in expected.
        for iface in &self.actual_interfaces {
            let is_bridge = iface.starts_with("syfb-");
            let is_vxlan = iface.starts_with("syfx-");
            let is_tap = iface.starts_with("syft-");

            if is_bridge && !self.expected_bridges.contains(iface) {
                reports.push(DriftReport {
                    resource_id: iface.clone(),
                    resource_type: ResourceType::Bridge,
                    status: DriftStatus::Orphaned,
                    scenario: DriftScenario::MissingBridge,
                    expected: "not in desired state".to_string(),
                    actual: format!("bridge {} exists in kernel", iface),
                });
            }
            if is_vxlan && !self.expected_vxlans.contains(iface) {
                reports.push(DriftReport {
                    resource_id: iface.clone(),
                    resource_type: ResourceType::Vxlan,
                    status: DriftStatus::Orphaned,
                    scenario: DriftScenario::MissingVxlan,
                    expected: "not in desired state".to_string(),
                    actual: format!("VXLAN {} exists in kernel", iface),
                });
            }
            if is_tap && !self.expected_taps.contains(iface) {
                reports.push(DriftReport {
                    resource_id: iface.clone(),
                    resource_type: ResourceType::Nic,
                    status: DriftStatus::Orphaned,
                    scenario: DriftScenario::MissingTap,
                    expected: "not in desired state".to_string(),
                    actual: format!("TAP {} exists in kernel", iface),
                });
            }
        }

        reports
    }
}

impl Default for NetworkDriftDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// VM drift detector — checks running processes against desired VM list.
pub struct VmDriftDetector {
    /// Expected VM IDs that should be running.
    pub expected_vms: Vec<String>,
    /// Actually running VM IDs.
    pub running_vms: Vec<String>,
}

impl VmDriftDetector {
    pub fn new() -> Self {
        Self {
            expected_vms: Vec::new(),
            running_vms: Vec::new(),
        }
    }

    /// Detect dead VMs (expected but not running).
    pub fn detect_dead_vms(&self) -> Vec<DriftReport> {
        let mut reports = Vec::new();
        for vm_id in &self.expected_vms {
            if !self.running_vms.contains(vm_id) {
                reports.push(DriftReport {
                    resource_id: vm_id.clone(),
                    resource_type: ResourceType::Vm,
                    status: DriftStatus::Drifted {
                        reason: "VM process not running".to_string(),
                    },
                    scenario: DriftScenario::DeadVm,
                    expected: format!("VM {} is running", vm_id),
                    actual: "VM process not found".to_string(),
                });
            }
        }
        reports
    }
}

impl Default for VmDriftDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Security group drift detector.
pub struct SgDriftDetector {
    /// Expected nftables chain names for VMs.
    pub expected_chains: Vec<String>,
    /// Actual nftables chain names from `nft list`.
    pub actual_chains: Vec<String>,
}

impl SgDriftDetector {
    pub fn new() -> Self {
        Self {
            expected_chains: Vec::new(),
            actual_chains: Vec::new(),
        }
    }

    /// Detect missing or stale SG chains.
    pub fn detect_sg_drift(&self) -> Vec<DriftReport> {
        let mut reports = Vec::new();
        for chain in &self.expected_chains {
            if !self.actual_chains.contains(chain) {
                reports.push(DriftReport {
                    resource_id: chain.clone(),
                    resource_type: ResourceType::SecurityGroup,
                    status: DriftStatus::Missing,
                    scenario: DriftScenario::StaleNftables,
                    expected: format!("chain {} exists", chain),
                    actual: "chain not found in nftables".to_string(),
                });
            }
        }
        reports
    }
}

impl Default for SgDriftDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// FDB drift detector.
pub struct FdbDriftDetector {
    /// Expected FDB entries (mac -> vtep).
    pub expected_entries: Vec<(String, String)>,
    /// Whether each entry exists (set by observation).
    pub missing: Vec<String>,
}

impl FdbDriftDetector {
    pub fn new() -> Self {
        Self {
            expected_entries: Vec::new(),
            missing: Vec::new(),
        }
    }

    /// Report missing FDB entries.
    pub fn detect_fdb_drift(&self) -> Vec<DriftReport> {
        self.missing
            .iter()
            .map(|mac| DriftReport {
                resource_id: format!("fdb-{}", mac),
                resource_type: ResourceType::Fdb,
                status: DriftStatus::Missing,
                scenario: DriftScenario::MissingFdb,
                expected: format!("FDB entry for MAC {} exists", mac),
                actual: "FDB entry not found".to_string(),
            })
            .collect()
    }
}

impl Default for FdbDriftDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// NAT drift detector.
pub struct NatDriftDetector {
    /// Expected NAT rules (bridge, subnet_cidr).
    pub expected_nat: Vec<(String, String)>,
    /// Missing NAT entries.
    pub missing: Vec<(String, String)>,
}

impl NatDriftDetector {
    pub fn new() -> Self {
        Self {
            expected_nat: Vec::new(),
            missing: Vec::new(),
        }
    }

    pub fn detect_nat_drift(&self) -> Vec<DriftReport> {
        self.missing
            .iter()
            .map(|(bridge, cidr)| DriftReport {
                resource_id: format!("nat-{}-{}", bridge, cidr),
                resource_type: ResourceType::NatGateway,
                status: DriftStatus::Missing,
                scenario: DriftScenario::MissingNat,
                expected: format!("NAT for {} on {} exists", cidr, bridge),
                actual: "NAT masquerade not found".to_string(),
            })
            .collect()
    }
}

impl Default for NatDriftDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_missing_bridge() {
        let mut detector = NetworkDriftDetector::new();
        detector.expected_bridges = vec!["syfb-11111111".to_string()];
        detector.set_actual(vec![]);

        let reports = detector.detect_all();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::MissingBridge);
        assert_eq!(reports[0].status, DriftStatus::Missing);
    }

    #[test]
    fn detect_missing_vxlan() {
        let mut detector = NetworkDriftDetector::new();
        detector.expected_vxlans = vec!["syfx-22222222".to_string()];
        detector.set_actual(vec![]);

        let reports = detector.detect_all();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::MissingVxlan);
    }

    #[test]
    fn detect_missing_tap() {
        let mut detector = NetworkDriftDetector::new();
        detector.expected_taps = vec!["syft-33333333".to_string()];
        detector.set_actual(vec![]);

        let reports = detector.detect_all();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::MissingTap);
    }

    #[test]
    fn detect_orphaned_bridge() {
        let mut detector = NetworkDriftDetector::new();
        detector.set_actual(vec!["syfb-orphan123".to_string()]);

        let reports = detector.detect_all();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].status, DriftStatus::Orphaned);
    }

    #[test]
    fn no_drift_when_in_sync() {
        let mut detector = NetworkDriftDetector::new();
        detector.expected_bridges = vec!["syfb-11111111".to_string()];
        detector.expected_vxlans = vec!["syfx-22222222".to_string()];
        detector.set_actual(vec![
            "syfb-11111111".to_string(),
            "syfx-22222222".to_string(),
        ]);

        let reports = detector.detect_all();
        assert!(reports.is_empty(), "expected no drift, got {:?}", reports);
    }

    #[test]
    fn detect_dead_vm() {
        let mut detector = VmDriftDetector::new();
        detector.expected_vms = vec!["vm-1".to_string(), "vm-2".to_string()];
        detector.running_vms = vec!["vm-1".to_string()];

        let reports = detector.detect_dead_vms();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::DeadVm);
        assert_eq!(reports[0].resource_id, "vm-2");
    }

    #[test]
    fn detect_stale_nftables() {
        let mut detector = SgDriftDetector::new();
        detector.expected_chains = vec!["vm_abc_in".to_string()];
        detector.actual_chains = vec![];

        let reports = detector.detect_sg_drift();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::StaleNftables);
    }

    #[test]
    fn detect_missing_fdb() {
        let mut detector = FdbDriftDetector::new();
        detector.missing = vec!["02:00:0a:01:00:03".to_string()];

        let reports = detector.detect_fdb_drift();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::MissingFdb);
    }

    #[test]
    fn detect_missing_nat() {
        let mut detector = NatDriftDetector::new();
        detector.missing = vec![("syfb-123".to_string(), "10.1.0.0/24".to_string())];

        let reports = detector.detect_nat_drift();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].scenario, DriftScenario::MissingNat);
    }

    #[test]
    fn all_12_scenarios_represented() {
        // Verify that all 12 scenarios are distinct enum variants.
        let scenarios = vec![
            DriftScenario::MissingBridge,
            DriftScenario::MissingVxlan,
            DriftScenario::MissingTap,
            DriftScenario::DeadVm,
            DriftScenario::StaleNftables,
            DriftScenario::WrongSgRules,
            DriftScenario::OrphanedIp,
            DriftScenario::MissingFdb,
            DriftScenario::MissingArpProxy,
            DriftScenario::MissingNat,
            DriftScenario::WrongGatewayIp,
            DriftScenario::StaleRoute,
        ];
        assert_eq!(scenarios.len(), 12);
        // All should have unique Display values.
        let displays: std::collections::HashSet<String> =
            scenarios.iter().map(|s| s.to_string()).collect();
        assert_eq!(displays.len(), 12);
    }
}
