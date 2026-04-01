//! Network setup integration — wires overlay networking into the VM lifecycle.
//!
//! `NetworkSetup` is the bridge between compute and the org/overlay layers. It
//! resolves subnets, allocates IPs via IPAM, creates bridges/VXLAN/TAP
//! interfaces via the `NetworkBackend` trait, applies firewall rules, and stores
//! VM placements. On failure, it rolls back all partially-created resources.

use std::sync::Arc;

use tracing::{info, warn};

use syfrah_org::ipam::IpamStore;
use syfrah_org::store::OrgStore;
use syfrah_org::types::{
    NetworkInterface, NicId, PlacementAction, ResourceState, SecurityGroupId, Subnet, VmPlacement,
    Vpc,
};
use syfrah_org::PlacementStore;
use syfrah_org::SgRuleStore;
use syfrah_overlay::backend::NetworkBackend;
use syfrah_overlay::sg_nft;

use crate::error::ComputeError;
use crate::image::types::CloudInitNetworkConfig;
use crate::types::NetworkConfig;

// ── Constants ────────────────────────────────────────────────────────

/// VXLAN overlay MTU: accounts for VXLAN (50B) + WireGuard (80B) overhead.
const OVERLAY_MTU: u16 = 1350;

/// VXLAN UDP port.
const VXLAN_PORT: u16 = 4789;

/// DNS nameservers injected into guest network config.
const DNS_SERVERS: &[&str] = &["8.8.8.8", "1.1.1.1"];

// ── Result of a successful network setup ──────────────────────────────

/// Everything the caller needs after network setup succeeds.
#[derive(Debug)]
pub struct NetworkSetupResult {
    /// TAP device name for the VM, or host-side veth name for containers.
    pub tap_name: String,
    /// Container-side veth name (only set for containers, moved into netns).
    pub container_veth: Option<String>,
    /// MAC address assigned by IPAM.
    pub mac: String,
    /// Allocated IP address.
    pub ip: String,
    /// Subnet CIDR (e.g. "10.0.1.0/24").
    pub subnet_cidr: String,
    /// Gateway IP.
    pub gateway: String,
    /// Prefix length parsed from the subnet CIDR.
    pub prefix_len: u8,
    /// Network config for Cloud Hypervisor.
    pub network_config: NetworkConfig,
    /// Cloud-init network config for the config-drive.
    pub cloud_init_network: CloudInitNetworkConfig,
    /// The VM placement record (for FDB distribution).
    pub placement: VmPlacement,
}

// ── NetworkSetup ─────────────────────────────────────────────────────

/// Orchestrates network setup for VM creation.
///
/// Holds references to the org store, IPAM store, placement store, and the
/// network backend. Designed to be called from `VmManager::create_vm()`.
pub struct NetworkSetup<B: NetworkBackend + ?Sized> {
    org_store: Arc<OrgStore>,
    ipam_store: Arc<IpamStore>,
    placement_store: Arc<PlacementStore>,
    backend: Arc<B>,
    /// This node's fabric IPv6 address (for VXLAN local IP and placement).
    local_node: String,
    /// SG rule store — when present, SG-based nftables rules are applied.
    sg_rule_store: Option<Arc<SgRuleStore>>,
}

impl<B: NetworkBackend + ?Sized> NetworkSetup<B> {
    /// Create a new `NetworkSetup` with the given dependencies.
    pub fn new(
        org_store: Arc<OrgStore>,
        ipam_store: Arc<IpamStore>,
        placement_store: Arc<PlacementStore>,
        backend: Arc<B>,
        local_node: String,
    ) -> Self {
        Self {
            org_store,
            ipam_store,
            placement_store,
            backend,
            local_node,
            sg_rule_store: None,
        }
    }

    /// Attach an SG rule store so that SG-based nftables rules are applied
    /// for VMs with security groups.
    pub fn with_sg_rule_store(mut self, store: Arc<SgRuleStore>) -> Self {
        self.sg_rule_store = Some(store);
        self
    }

    /// Run the full network setup sequence for a VM.
    ///
    /// Steps (per ADR-001 Step 9):
    /// 1. Resolve subnet -> VPC
    /// 2. Allocate IP from IPAM
    /// 3. Ensure VXLAN interface
    /// 4. Ensure bridge + gateway IP
    /// 5. Create TAP, attach to bridge
    /// 6. Apply nftables (anti-spoofing, default rules)
    /// 7. Apply NAT (SNAT masquerade)
    /// 8. Store placement + local FDB
    ///
    /// On failure after step 2, releases the allocated IP and cleans up any
    /// partially-created resources.
    pub async fn setup(
        &self,
        vm_id: &str,
        subnet_name: &str,
        is_container: bool,
    ) -> Result<NetworkSetupResult, ComputeError> {
        self.setup_with_sg(vm_id, subnet_name, is_container, &[])
            .await
    }

    /// Run network setup with security group support.
    ///
    /// When `security_groups` is non-empty, SG-based nftables rules are
    /// applied instead of the hardcoded anti-spoofing rules. Existing VMs
    /// (empty `security_groups`) get the legacy `apply_vm_rules` behavior
    /// plus a "default" SG assignment.
    pub async fn setup_with_sg(
        &self,
        vm_id: &str,
        subnet_name: &str,
        is_container: bool,
        security_groups: &[String],
    ) -> Result<NetworkSetupResult, ComputeError> {
        // -- 1. Resolve subnet ------------------------------------------------
        let (subnet, vpc) = self.resolve_subnet(subnet_name)?;
        let subnet_id = subnet.id.0.clone();
        let subnet_cidr = subnet.cidr.clone();
        let gateway = subnet.gateway.clone();
        let vpc_id = vpc.id.0.clone();
        let vni = vpc.vni;

        info!(
            vm_id,
            subnet = subnet_name,
            vpc = %vpc.name,
            vni,
            "resolved subnet for VM"
        );

        // -- 2. Allocate IP ---------------------------------------------------
        let allocation = self
            .ipam_store
            .reserve_ip(&subnet_id, &subnet_cidr)
            .map_err(|e| ComputeError::NetworkSetup(format!("IPAM reservation failed: {e}")))?;

        let ip = allocation.ip.clone();
        let mac = allocation.mac.clone();

        info!(vm_id, %ip, %mac, "IP reserved from IPAM");

        // From here on, any failure must release the IP.
        match self
            .setup_network_resources(
                vm_id,
                &vpc_id,
                vni,
                &subnet_id,
                &subnet_cidr,
                &gateway,
                &ip,
                &mac,
                is_container,
                security_groups,
            )
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                // Rollback: release IP allocation.
                warn!(vm_id, %ip, error = %e, "network setup failed, rolling back IP");
                if let Err(release_err) = self.ipam_store.release_ip(&subnet_id, &subnet_cidr, &ip)
                {
                    warn!(
                        vm_id, %ip,
                        error = %release_err,
                        "failed to release IP during rollback"
                    );
                }
                Err(e)
            }
        }
    }

    /// Internal: set up all network resources after IP allocation.
    ///
    /// Separated so the caller can wrap it with rollback logic.
    #[allow(clippy::too_many_arguments)]
    async fn setup_network_resources(
        &self,
        vm_id: &str,
        vpc_id: &str,
        vni: u32,
        subnet_id: &str,
        subnet_cidr: &str,
        gateway: &str,
        ip: &str,
        mac: &str,
        is_container: bool,
        security_groups: &[String],
    ) -> Result<NetworkSetupResult, ComputeError> {
        let bridge_name = syfrah_overlay::naming::bridge_name(vpc_id);
        let vxlan_name = syfrah_overlay::naming::vxlan_name(vpc_id);

        // Parse prefix length from CIDR.
        let prefix_len = parse_prefix_len(subnet_cidr)?;

        // -- 3. Ensure VXLAN --------------------------------------------------
        self.backend
            .create_vxlan(&vxlan_name, vni, &self.local_node, VXLAN_PORT)
            .await
            .map_err(|e| ComputeError::NetworkSetup(format!("VXLAN creation failed: {e}")))?;

        // -- 4. Ensure bridge + attach VXLAN + gateway IP ---------------------
        self.backend
            .create_bridge(&bridge_name)
            .await
            .map_err(|e| ComputeError::NetworkSetup(format!("bridge creation failed: {e}")))?;

        self.backend
            .attach_to_bridge(&vxlan_name, &bridge_name)
            .await
            .map_err(|e| {
                ComputeError::NetworkSetup(format!("attach VXLAN to bridge failed: {e}"))
            })?;

        self.backend
            .add_bridge_ip(&bridge_name, gateway, prefix_len)
            .await
            .map_err(|e| ComputeError::NetworkSetup(format!("add bridge IP failed: {e}")))?;

        // -- 5. Create host-side interface and attach to bridge ----------------
        // For VMs: create a TAP device.
        // For containers: create a veth pair (host end attaches to bridge,
        //   container end will be moved into the netns after crun creates it).
        let (host_iface, container_veth) = if is_container {
            let host_veth = syfrah_overlay::naming::veth_host_name(vm_id);
            let cont_veth = syfrah_overlay::naming::veth_container_name(vm_id);
            self.backend
                .create_veth_pair(&host_veth, &cont_veth)
                .await
                .map_err(|e| {
                    ComputeError::NetworkSetup(format!("veth pair creation failed: {e}"))
                })?;
            (host_veth, Some(cont_veth))
        } else {
            let tap_name = syfrah_overlay::naming::tap_name(vm_id);
            self.backend
                .create_tap(&tap_name)
                .await
                .map_err(|e| ComputeError::NetworkSetup(format!("TAP creation failed: {e}")))?;
            (tap_name, None)
        };

        self.backend
            .attach_to_bridge(&host_iface, &bridge_name)
            .await
            .map_err(|e| {
                ComputeError::NetworkSetup(format!(
                    "attach {} to bridge failed: {e}",
                    if is_container { "veth" } else { "TAP" }
                ))
            })?;

        // -- 6. Apply nftables rules ------------------------------------------
        // Use SG-based rules when security groups are specified and a rule
        // store is available; otherwise fall back to legacy hardcoded rules.
        let sg_applied = if !security_groups.is_empty() {
            if let Some(ref rule_store) = self.sg_rule_store {
                info!(
                    vm_id,
                    sgs = ?security_groups,
                    "applying SG-based nftables rules"
                );

                // Collect all rules for the VM's security groups.
                let sg_ids: Vec<syfrah_org::types::SecurityGroupId> = security_groups
                    .iter()
                    .filter_map(|name| {
                        self.org_store
                            .find_sg_by_name(name)
                            .ok()
                            .flatten()
                            .map(|sg| sg.id)
                    })
                    .collect();

                let mut overlay_rules = Vec::new();
                for sg_id in &sg_ids {
                    if let Ok(rules) = rule_store.list_rules_by_sg(sg_id) {
                        for r in rules {
                            overlay_rules.push(convert_org_rule_to_overlay(&r));
                        }
                    }
                }

                // Build the NIC for sg_nft (overlay type).
                let nft_nic = sg_nft::NetworkInterface {
                    id: sg_nft::NicId(format!("nic-{vm_id}")),
                    vm_id: vm_id.to_string(),
                    private_ip: ip
                        .parse()
                        .map_err(|e| ComputeError::NetworkSetup(format!("bad IP: {e}")))?,
                    mac: mac.to_string(),
                    security_groups: sg_ids
                        .iter()
                        .map(|id| syfrah_overlay::sg::SecurityGroupId(id.0.clone()))
                        .collect(),
                };

                let sg_ip_map = std::collections::HashMap::new();
                sg_nft::apply_sg_for_vm(&nft_nic, &overlay_rules, &sg_ip_map).map_err(|e| {
                    ComputeError::NetworkSetup(format!("SG nftables rules failed: {e}"))
                })?;

                true
            } else {
                false
            }
        } else {
            false
        };

        // Fall back to legacy hardcoded rules when SG rules were not applied.
        if !sg_applied {
            self.backend
                .apply_vm_rules(&host_iface, mac, ip)
                .await
                .map_err(|e| ComputeError::NetworkSetup(format!("nftables rules failed: {e}")))?;
        }

        // -- 7. Apply NAT (only if an active NAT Gateway exists) ---------------
        // Check if the VPC has an active NAT Gateway with a route pointing to it.
        // If no NAT GW: VMs have private networking only (no internet egress).
        let has_nat_gw = self.has_active_nat_gw(vpc_id);
        if has_nat_gw {
            self.backend
                .apply_nat(&bridge_name, subnet_cidr)
                .await
                .map_err(|e| ComputeError::NetworkSetup(format!("NAT setup failed: {e}")))?;
        } else {
            warn!(
                vm_id,
                vpc_id, "no NAT gateway — VMs will not have internet egress"
            );
        }

        // -- 8. Store placement -----------------------------------------------
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let placement = VmPlacement {
            vpc_id: vpc_id.to_string(),
            vm_id: vm_id.to_string(),
            vm_mac: mac.to_string(),
            vm_ip: ip.to_string(),
            subnet_id: subnet_id.to_string(),
            hypervisor_id: self.local_node.clone(),
            action: PlacementAction::Add,
            created_at: now,
            placement_generation: 1,
        };

        self.placement_store
            .add_placement(&placement)
            .map_err(|e| ComputeError::NetworkSetup(format!("placement store failed: {e}")))?;

        // -- 9. Create NIC record ------------------------------------------------
        // Build security group list: use specified SGs or default to "default".
        let sg_ids: Vec<SecurityGroupId> = if security_groups.is_empty() {
            // Look up the default SG for this VPC.
            let default_sg_key = format!("sg-default-{vpc_id}");
            vec![SecurityGroupId(default_sg_key)]
        } else {
            security_groups
                .iter()
                .filter_map(|name| {
                    self.org_store
                        .find_sg_by_name(name)
                        .ok()
                        .flatten()
                        .map(|sg| sg.id)
                })
                .collect()
        };

        let nic_id = format!("nic-{vm_id}");
        let nic = NetworkInterface {
            id: NicId(nic_id.clone()),
            name: format!("eth0-{vm_id}"),
            vm_id: Some(vm_id.to_string()),
            subnet_id: subnet_id.to_string(),
            vpc_id: vpc_id.to_string(),
            private_ip: ip.to_string(),
            mac: mac.to_string(),
            security_groups: sg_ids,
            state: ResourceState::Active,
            created_at: now,
        };

        if let Err(e) = self.org_store.create_nic(&nic) {
            warn!(vm_id, error = %e, "failed to create NIC record (non-fatal)");
        } else {
            info!(vm_id, nic_id = %nic_id, "NIC record created");
        }

        info!(
            vm_id,
            %ip, %mac,
            iface = %host_iface,
            bridge = %bridge_name,
            is_container,
            "network setup complete"
        );

        // Build result
        let network_config = NetworkConfig {
            tap_name: host_iface.clone(),
            mac: Some(mac.to_string()),
        };

        let cloud_init_network = CloudInitNetworkConfig {
            ip: ip.to_string(),
            prefix_len,
            gateway: gateway.to_string(),
            mtu: OVERLAY_MTU,
            dns: DNS_SERVERS.iter().map(|s| s.to_string()).collect(),
        };

        Ok(NetworkSetupResult {
            tap_name: host_iface,
            container_veth,
            mac: mac.to_string(),
            ip: ip.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
            gateway: gateway.to_string(),
            prefix_len,
            network_config,
            cloud_init_network,
            placement,
        })
    }

    /// Check if the VPC has an active NAT Gateway with a route targeting it.
    fn has_active_nat_gw(&self, vpc_id: &str) -> bool {
        let vpc_id_typed = syfrah_org::types::VpcId(vpc_id.to_string());

        // Check for any active NAT GWs in this VPC.
        let gws = match self.org_store.list_nat_gws_by_vpc(&vpc_id_typed) {
            Ok(g) => g,
            Err(_) => return false,
        };

        gws.iter()
            .any(|gw| gw.state == syfrah_org::types::ResourceState::Active)
    }

    /// Resolve a subnet name to its `Subnet` and parent `Vpc`.
    fn resolve_subnet(&self, subnet_name: &str) -> Result<(Subnet, Vpc), ComputeError> {
        let matches = self
            .org_store
            .find_subnets_by_name(subnet_name)
            .map_err(|e| ComputeError::NetworkSetup(format!("subnet lookup failed: {e}")))?;

        if matches.is_empty() {
            return Err(ComputeError::NetworkSetup(format!(
                "subnet '{subnet_name}' not found"
            )));
        }
        if matches.len() > 1 {
            return Err(ComputeError::NetworkSetup(format!(
                "ambiguous subnet name '{subnet_name}': found in {} VPCs, specify --vpc",
                matches.len()
            )));
        }

        let (_vpc_name, subnet) = matches.into_iter().next().unwrap();
        let vpc = self
            .org_store
            .get_vpc_by_id(&subnet.vpc_id)
            .map_err(|e| ComputeError::NetworkSetup(format!("VPC lookup failed: {e}")))?
            .ok_or_else(|| {
                ComputeError::NetworkSetup(format!(
                    "VPC '{}' referenced by subnet not found",
                    subnet.vpc_id
                ))
            })?;

        Ok((subnet, vpc))
    }

    /// Mark the IPAM allocation as assigned after successful VM boot.
    pub fn mark_assigned(
        &self,
        subnet_id: &str,
        ip: &str,
        vm_id: &str,
    ) -> Result<(), ComputeError> {
        self.ipam_store
            .assign_ip(subnet_id, ip, vm_id)
            .map_err(|e| ComputeError::NetworkSetup(format!("IPAM assign failed: {e}")))?;
        Ok(())
    }

    /// Tear down network resources for a VM. Called on VM delete or as part of
    /// rollback when VM creation fails after network setup.
    pub async fn teardown(
        &self,
        vm_id: &str,
        vpc_id: &str,
        subnet_id: &str,
        subnet_cidr: &str,
        ip: &str,
        tap_name: &str,
    ) -> Result<(), ComputeError> {
        let _bridge_name = syfrah_overlay::naming::bridge_name(vpc_id);

        // Remove nftables rules (best-effort).
        if let Err(e) = self.backend.remove_vm_rules(tap_name).await {
            warn!(vm_id, error = %e, "failed to remove VM rules");
        }

        // Delete TAP.
        if let Err(e) = self.backend.delete_tap(tap_name).await {
            warn!(vm_id, error = %e, "failed to delete TAP");
        }

        // Remove placement.
        if let Err(e) = self.placement_store.remove_placement(vpc_id, vm_id) {
            warn!(vm_id, error = %e, "failed to remove placement");
        }

        // Release IP.
        if let Err(e) = self.ipam_store.release_ip(subnet_id, subnet_cidr, ip) {
            warn!(vm_id, error = %e, "failed to release IP");
        }

        // Delete NIC record (best-effort).
        let nic_id = format!("nic-{vm_id}");
        if let Ok(Some(_)) = self.org_store.get_nic(&nic_id) {
            if let Err(e) = self.org_store.delete_nic(&nic_id) {
                warn!(vm_id, error = %e, "failed to delete NIC record");
            }
        }

        info!(vm_id, "network teardown complete");

        // Note: bridge and VXLAN are left in place — they are shared resources.
        // They would only be removed if no more VMs exist on this VPC on this node
        // (handled by the reconciliation loop, out of scope for this PR).

        Ok(())
    }
}

/// Parse the prefix length from a CIDR string (e.g. "10.0.1.0/24" -> 24).
fn parse_prefix_len(cidr: &str) -> Result<u8, ComputeError> {
    let parts: Vec<&str> = cidr.split('/').collect();
    if parts.len() != 2 {
        return Err(ComputeError::NetworkSetup(format!("invalid CIDR: {cidr}")));
    }
    parts[1]
        .parse::<u8>()
        .map_err(|_| ComputeError::NetworkSetup(format!("invalid prefix length in CIDR: {cidr}")))
}

/// Convert an org-layer `SecurityGroupRule` into the overlay-layer type
/// used by `sg_nft`.
fn convert_org_rule_to_overlay(
    r: &syfrah_org::types::SecurityGroupRule,
) -> syfrah_overlay::sg::SecurityGroupRule {
    use syfrah_overlay::sg as ov;

    let direction = match r.direction {
        syfrah_org::types::Direction::Ingress => ov::Direction::Ingress,
        syfrah_org::types::Direction::Egress => ov::Direction::Egress,
    };

    let protocol = match r.protocol {
        syfrah_org::types::Protocol::Tcp => ov::Protocol::Tcp,
        syfrah_org::types::Protocol::Udp => ov::Protocol::Udp,
        syfrah_org::types::Protocol::Icmp => ov::Protocol::Icmp,
        syfrah_org::types::Protocol::All => ov::Protocol::All,
    };

    let port_range = r.port_range.as_ref().map(|pr| ov::PortRange {
        from: pr.from,
        to: pr.to,
    });

    let source = match &r.source {
        syfrah_org::types::RuleSource::Cidr(cidr) => ov::TrafficSource::Cidr(cidr.clone()),
        syfrah_org::types::RuleSource::SecurityGroup(sg_id) => {
            ov::TrafficSource::SecurityGroup(sg_id.0.clone())
        }
    };

    ov::SecurityGroupRule {
        id: ov::RuleId(r.id.0.clone()),
        sg_id: ov::SecurityGroupId(r.sg_id.0.clone()),
        direction,
        protocol,
        port_range,
        source,
        priority: r.priority,
        description: r.description.clone().unwrap_or_default(),
        created_at: 0,
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use syfrah_org::ipam::IpamStore;
    use syfrah_org::types::{EnvironmentId, ProjectId, VpcOwner};
    use syfrah_org::PlacementStore;
    use syfrah_overlay::MockBackend;
    use tempfile::TempDir;

    const SUBNET_NAME: &str = "frontend";
    const SUBNET_CIDR: &str = "10.0.1.0/24";
    const GATEWAY: &str = "10.0.1.1";
    const VPC_NAME: &str = "default";
    const VPC_CIDR: &str = "10.0.0.0/16";
    const ORG_NAME: &str = "acme";
    const PROJECT_NAME: &str = "backend";
    const ENV_NAME: &str = "production";
    const LOCAL_NODE: &str = "fd12:3456:7800::1";

    struct TestHarness {
        _dir: TempDir,
        org_store: Arc<OrgStore>,
        ipam_store: Arc<IpamStore>,
        placement_store: Arc<PlacementStore>,
        backend: Arc<MockBackend>,
    }

    impl TestHarness {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let org_db = syfrah_state::LayerDb::open_at(&dir.path().join("org.redb")).unwrap();
            let ipam_db = syfrah_state::LayerDb::open_at(&dir.path().join("ipam.redb")).unwrap();
            let placement_db =
                syfrah_state::LayerDb::open_at(&dir.path().join("placement.redb")).unwrap();

            let org_store = Arc::new(OrgStore::new(org_db));
            let ipam_store = Arc::new(IpamStore::new(ipam_db));
            let placement_store = Arc::new(PlacementStore::new(placement_db));
            let backend = Arc::new(MockBackend::new());

            // Set up org hierarchy: org -> project -> env -> vpc -> subnet
            org_store.create(ORG_NAME).unwrap();
            org_store.create_project(ORG_NAME, PROJECT_NAME).unwrap();

            let project_id = ProjectId(format!("{ORG_NAME}/{PROJECT_NAME}"));
            let env_id = EnvironmentId(format!("{ORG_NAME}/{PROJECT_NAME}/{ENV_NAME}"));
            org_store
                .create_env(
                    ORG_NAME,
                    PROJECT_NAME,
                    ENV_NAME,
                    None,
                    false,
                    HashMap::new(),
                )
                .unwrap();
            org_store
                .create_vpc(VPC_NAME, VPC_CIDR, VpcOwner::Project(project_id), false)
                .unwrap();
            org_store
                .create_subnet(VPC_NAME, &env_id, SUBNET_NAME, Some(SUBNET_CIDR))
                .unwrap();

            Self {
                _dir: dir,
                org_store,
                ipam_store,
                placement_store,
                backend,
            }
        }

        fn network_setup(&self) -> NetworkSetup<MockBackend> {
            NetworkSetup::new(
                self.org_store.clone(),
                self.ipam_store.clone(),
                self.placement_store.clone(),
                self.backend.clone(),
                LOCAL_NODE.to_string(),
            )
        }
    }

    #[tokio::test]
    async fn create_vm_with_network() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // Verify IP and MAC assigned
        assert_eq!(result.ip, "10.0.1.3");
        assert_eq!(result.mac, "02:00:0a:00:01:03");
        assert_eq!(result.gateway, GATEWAY);
        assert_eq!(result.subnet_cidr, SUBNET_CIDR);

        // Verify calls happened in the right sequence
        let calls = h.backend.calls();
        let call_names: Vec<&str> = calls.iter().map(|c| c.split('(').next().unwrap()).collect();

        assert!(
            call_names.contains(&"create_vxlan"),
            "VXLAN must be created"
        );
        assert!(
            call_names.contains(&"create_bridge"),
            "bridge must be created"
        );
        assert!(
            call_names.contains(&"attach_to_bridge"),
            "VXLAN must be attached"
        );
        assert!(
            call_names.contains(&"add_bridge_ip"),
            "gateway IP must be added"
        );
        assert!(call_names.contains(&"create_tap"), "TAP must be created");
        assert!(
            call_names.contains(&"apply_vm_rules"),
            "nftables must be applied"
        );
        // NAT is only applied when an active NAT Gateway exists in the VPC.
        // In this test there is no NAT GW, so apply_nat should NOT be called.
        assert!(
            !call_names.contains(&"apply_nat"),
            "NAT must NOT be applied without a NAT Gateway"
        );
    }

    #[tokio::test]
    async fn ip_allocated() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // Verify IPAM was called — IP is the first allocatable (.3)
        assert_eq!(result.ip, "10.0.1.3");

        // Verify allocation exists in IPAM
        let alloc = h
            .ipam_store
            .get_allocation(&result.placement.subnet_id, &result.ip)
            .unwrap();
        assert!(alloc.is_some(), "allocation must exist in IPAM");
    }

    #[tokio::test]
    async fn tap_created() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        let expected_tap = syfrah_overlay::naming::tap_name("web-1");
        assert_eq!(result.tap_name, expected_tap);

        // Verify TAP was created and attached to bridge
        let calls = h.backend.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("create_tap({expected_tap})")));
        assert!(calls
            .iter()
            .any(|c| c.starts_with(&format!("attach_to_bridge({expected_tap}"))));
    }

    #[tokio::test]
    async fn bridge_created() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let _result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        let calls = h.backend.calls();
        // Bridge should be created with the VPC ID
        assert!(
            calls.iter().any(|c| c.starts_with(&format!(
                "create_bridge({}",
                syfrah_overlay::naming::BRIDGE_PREFIX
            ))),
            "bridge must be created"
        );
    }

    #[tokio::test]
    async fn fdb_announced() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // Verify placement was stored
        let stored = h
            .placement_store
            .get_placement(&result.placement.vpc_id, "web-1")
            .unwrap();
        assert!(stored.is_some(), "placement must be stored");
        let p = stored.unwrap();
        assert_eq!(p.vm_ip, "10.0.1.3");
        assert_eq!(p.vm_mac, "02:00:0a:00:01:03");
        assert_eq!(p.hypervisor_id, LOCAL_NODE);
    }

    #[tokio::test]
    async fn rollback_on_failure() {
        let h = TestHarness::new();

        // Make TAP creation fail — this is after IP allocation
        h.backend.set_fail("create_tap");

        let ns = h.network_setup();
        let result = ns.setup("web-1", SUBNET_NAME, false).await;

        assert!(result.is_err(), "setup must fail when TAP creation fails");

        // Verify the IP was rolled back — check that no allocations remain
        let subnet_matches = h.org_store.find_subnets_by_name(SUBNET_NAME).unwrap();
        let (_, subnet) = &subnet_matches[0];
        let allocations = h.ipam_store.list_allocations(&subnet.id.0).unwrap();
        assert!(
            allocations.is_empty(),
            "IP must be released on rollback, got {} allocations",
            allocations.len()
        );
    }

    #[tokio::test]
    async fn cloud_init_network_config() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // Verify cloud-init network config
        assert_eq!(result.cloud_init_network.ip, "10.0.1.3");
        assert_eq!(result.cloud_init_network.prefix_len, 24);
        assert_eq!(result.cloud_init_network.gateway, GATEWAY);
        assert_eq!(result.cloud_init_network.mtu, 1350);
        assert_eq!(result.cloud_init_network.dns.len(), 2);
    }

    #[tokio::test]
    async fn subnet_not_found() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", "nonexistent", false).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention not found: {err}"
        );
    }

    #[tokio::test]
    async fn mark_assigned_after_boot() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // Mark as assigned (happens after successful boot)
        ns.mark_assigned(&result.placement.subnet_id, &result.ip, "web-1")
            .unwrap();

        // Verify state changed to Assigned
        let alloc = h
            .ipam_store
            .get_allocation(&result.placement.subnet_id, &result.ip)
            .unwrap()
            .unwrap();
        assert_eq!(alloc.state, syfrah_org::AllocationState::Assigned);
        assert_eq!(alloc.vm_id, Some("web-1".to_string()));
    }

    #[tokio::test]
    async fn teardown_releases_resources() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();
        h.backend.reset();

        // Teardown
        ns.teardown(
            "web-1",
            &result.placement.vpc_id,
            &result.placement.subnet_id,
            &result.subnet_cidr,
            &result.ip,
            &result.tap_name,
        )
        .await
        .unwrap();

        // Verify cleanup calls
        let calls = h.backend.calls();
        assert!(calls.iter().any(|c| c.starts_with("remove_vm_rules(")));
        assert!(calls.iter().any(|c| c.starts_with("delete_tap(")));

        // Verify IP released from IPAM
        let alloc = h
            .ipam_store
            .get_allocation(&result.placement.subnet_id, &result.ip)
            .unwrap();
        assert!(
            alloc.is_none(),
            "IP allocation must be removed after teardown"
        );
    }

    #[tokio::test]
    async fn container_creates_veth_pair_not_tap() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, true).await.unwrap();

        // Container should get a veth pair, not a TAP.
        let expected_host = syfrah_overlay::naming::veth_host_name("web-1");
        let expected_cont = syfrah_overlay::naming::veth_container_name("web-1");
        assert_eq!(result.tap_name, expected_host);
        assert_eq!(result.container_veth, Some(expected_cont.clone()));
        assert!(result.prefix_len > 0, "prefix_len must be set");

        let calls = h.backend.calls();
        let call_names: Vec<&str> = calls.iter().map(|c| c.split('(').next().unwrap()).collect();

        assert!(
            call_names.contains(&"create_veth_pair"),
            "veth pair must be created for container"
        );
        assert!(
            !call_names.contains(&"create_tap"),
            "TAP must NOT be created for container"
        );
        assert!(
            call_names.contains(&"attach_to_bridge"),
            "host veth must be attached to bridge"
        );
        assert!(
            call_names.contains(&"apply_vm_rules"),
            "nftables must be applied on host veth"
        );

        // Verify the veth pair call has the right names
        assert!(calls
            .iter()
            .any(|c| c == &format!("create_veth_pair({expected_host}, {expected_cont})")));
    }

    #[tokio::test]
    async fn vm_creates_tap_not_veth() {
        let h = TestHarness::new();
        let ns = h.network_setup();

        let result = ns.setup("web-1", SUBNET_NAME, false).await.unwrap();

        // VM should get a TAP, not a veth pair.
        let expected_tap = syfrah_overlay::naming::tap_name("web-1");
        assert_eq!(result.tap_name, expected_tap);
        assert!(result.container_veth.is_none());

        let calls = h.backend.calls();
        let call_names: Vec<&str> = calls.iter().map(|c| c.split('(').next().unwrap()).collect();

        assert!(
            call_names.contains(&"create_tap"),
            "TAP must be created for VM"
        );
        assert!(
            !call_names.contains(&"create_veth_pair"),
            "veth pair must NOT be created for VM"
        );
    }
}
