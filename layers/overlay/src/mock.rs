use std::collections::HashSet;
use std::sync::Mutex;

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};

/// In-memory mock that records every call for test assertions.
///
/// Also tracks which interfaces exist so that [`NetworkBackend::list_interfaces`]
/// returns meaningful data during reconciliation tests.
pub struct MockBackend {
    calls: Mutex<Vec<String>>,
    /// Method names that should return an error (e.g. "add_fdb_entry").
    fail_methods: Mutex<HashSet<String>>,
    /// Simulated kernel interfaces (bridges, TAPs, VXLANs).
    interfaces: Mutex<HashSet<String>>,
    /// Simulated FDB entries per VXLAN: (mac, dst).
    fdb_entries: Mutex<Vec<(String, String, String)>>,
    /// Simulated ARP proxy entries per VXLAN: (ip, mac).
    arp_entries: Mutex<Vec<(String, String, String)>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_methods: Mutex::new(HashSet::new()),
            interfaces: Mutex::new(HashSet::new()),
            fdb_entries: Mutex::new(Vec::new()),
            arp_entries: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock poisoned").clone()
    }

    /// Clear recorded calls.
    pub fn reset(&self) {
        self.calls.lock().expect("lock poisoned").clear();
    }

    /// Make a specific method return an error on every invocation.
    pub fn set_fail(&self, method: &str) {
        self.fail_methods
            .lock()
            .expect("lock poisoned")
            .insert(method.to_string());
    }

    /// Pre-populate an interface so that `list_interfaces` returns it.
    pub fn add_interface(&self, name: &str) {
        self.interfaces
            .lock()
            .expect("lock poisoned")
            .insert(name.to_string());
    }

    /// Seed the mock with a list of kernel interfaces that `list_interfaces`
    /// will return (filtered by prefix).
    pub fn set_interfaces(&self, ifaces: Vec<String>) {
        let mut guard = self.interfaces.lock().expect("lock poisoned");
        guard.clear();
        for iface in ifaces {
            guard.insert(iface);
        }
    }

    /// Add a simulated FDB entry for testing list_fdb_entries.
    pub fn add_fdb(&self, vxlan: &str, mac: &str, dst: &str) {
        self.fdb_entries.lock().expect("lock poisoned").push((
            vxlan.to_string(),
            mac.to_string(),
            dst.to_string(),
        ));
    }

    /// Add a simulated ARP proxy entry for testing list_arp_entries.
    pub fn add_arp(&self, vxlan: &str, ip: &str, mac: &str) {
        self.arp_entries.lock().expect("lock poisoned").push((
            vxlan.to_string(),
            ip.to_string(),
            mac.to_string(),
        ));
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("lock poisoned").push(call);
    }

    fn should_fail(&self, method: &str) -> Result<()> {
        if self
            .fail_methods
            .lock()
            .expect("lock poisoned")
            .contains(method)
        {
            return Err(OverlayError::CommandFailed(format!(
                "{method} injected failure"
            )));
        }
        Ok(())
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NetworkBackend for MockBackend {
    // ── VXLAN ──────────────────────────────────────────────────────────

    async fn create_vxlan(&self, name: &str, vni: u32, local_ip: &str, port: u16) -> Result<()> {
        self.record(format!("create_vxlan({name}, {vni}, {local_ip}, {port})"));
        Ok(())
    }

    async fn delete_vxlan(&self, name: &str) -> Result<()> {
        self.record(format!("delete_vxlan({name})"));
        Ok(())
    }

    async fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: &str) -> Result<()> {
        self.should_fail("add_fdb_entry")?;
        self.record(format!("add_fdb_entry({bridge}, {mac}, {vtep})"));
        Ok(())
    }

    async fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()> {
        self.record(format!("remove_fdb_entry({bridge}, {mac})"));
        Ok(())
    }

    async fn add_arp_proxy(&self, vxlan: &str, ip: &str, mac: &str) -> Result<()> {
        self.should_fail("add_arp_proxy")?;
        self.record(format!("add_arp_proxy({vxlan}, {ip}, {mac})"));
        Ok(())
    }

    async fn remove_arp_proxy(&self, vxlan: &str, ip: &str) -> Result<()> {
        self.record(format!("remove_arp_proxy({vxlan}, {ip})"));
        Ok(())
    }

    // ── Bridge ─────────────────────────────────────────────────────────

    async fn create_bridge(&self, name: &str) -> Result<()> {
        self.record(format!("create_bridge({name})"));
        Ok(())
    }

    async fn add_bridge_ip(&self, bridge: &str, ip: &str, prefix_len: u8) -> Result<()> {
        self.record(format!("add_bridge_ip({bridge}, {ip}, {prefix_len})"));
        Ok(())
    }

    async fn remove_bridge_ip(&self, bridge: &str, ip: &str) -> Result<()> {
        self.record(format!("remove_bridge_ip({bridge}, {ip})"));
        Ok(())
    }

    async fn delete_bridge(&self, name: &str) -> Result<()> {
        self.record(format!("delete_bridge({name})"));
        Ok(())
    }

    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()> {
        self.record(format!("attach_to_bridge({interface}, {bridge})"));
        Ok(())
    }

    // ── TAP / veth ─────────────────────────────────────────────────────

    async fn create_tap(&self, name: &str) -> Result<()> {
        self.should_fail("create_tap")?;
        self.record(format!("create_tap({name})"));
        Ok(())
    }

    async fn delete_tap(&self, name: &str) -> Result<()> {
        self.should_fail("delete_tap")?;
        self.record(format!("delete_tap({name})"));
        Ok(())
    }

    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()> {
        self.record(format!("create_veth_pair({name_a}, {name_b})"));
        Ok(())
    }

    async fn move_to_netns(&self, iface: &str, pid: u32) -> Result<()> {
        self.should_fail("move_to_netns")?;
        self.record(format!("move_to_netns({iface}, {pid})"));
        Ok(())
    }

    async fn configure_netns(
        &self,
        pid: u32,
        iface: &str,
        ip: &str,
        prefix_len: u8,
        gateway: &str,
        mac: &str,
    ) -> Result<()> {
        self.should_fail("configure_netns")?;
        self.record(format!(
            "configure_netns({pid}, {iface}, {ip}, {prefix_len}, {gateway}, {mac})"
        ));
        Ok(())
    }

    // ── Firewall ───────────────────────────────────────────────────────

    async fn enable_br_netfilter(&self) -> Result<()> {
        self.record("enable_br_netfilter()".to_string());
        Ok(())
    }

    async fn apply_infra_protection(&self) -> Result<()> {
        self.record("apply_infra_protection()".to_string());
        Ok(())
    }

    async fn apply_sg_base_chain(&self) -> Result<()> {
        self.record("apply_sg_base_chain()".to_string());
        Ok(())
    }

    async fn apply_bridge_accept_rules(&self, bridge: &str) -> Result<()> {
        self.record(format!("apply_bridge_accept_rules({bridge})"));
        Ok(())
    }

    async fn apply_vm_rules(&self, tap: &str, mac: &str, ip: &str) -> Result<()> {
        self.record(format!("apply_vm_rules({tap}, {mac}, {ip})"));
        Ok(())
    }

    async fn remove_vm_rules(&self, tap: &str) -> Result<()> {
        self.should_fail("remove_vm_rules")?;
        self.record(format!("remove_vm_rules({tap})"));
        Ok(())
    }

    async fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()> {
        self.record(format!("apply_nat({bridge}, {subnet_cidr})"));
        Ok(())
    }

    async fn remove_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()> {
        self.record(format!("remove_nat({bridge}, {subnet_cidr})"));
        Ok(())
    }

    async fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        self.record(format!("apply_peering_rules({bridge_a}, {bridge_b})"));
        Ok(())
    }

    async fn remove_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        self.record(format!("remove_peering_rules({bridge_a}, {bridge_b})"));
        Ok(())
    }

    async fn link_exists(&self, name: &str) -> bool {
        self.record(format!("link_exists({name})"));
        self.interfaces
            .lock()
            .expect("lock poisoned")
            .contains(name)
    }

    async fn list_interfaces(&self, prefix: &str) -> Result<Vec<String>> {
        self.record(format!("list_interfaces({prefix})"));
        let interfaces = self.interfaces.lock().expect("lock poisoned");
        let mut matched: Vec<String> = interfaces
            .iter()
            .filter(|name| name.starts_with(prefix))
            .cloned()
            .collect();
        matched.sort();
        Ok(matched)
    }

    async fn list_fdb_entries(&self, vxlan: &str) -> Result<Vec<(String, String)>> {
        self.record(format!("list_fdb_entries({vxlan})"));
        let entries = self.fdb_entries.lock().expect("lock poisoned");
        Ok(entries
            .iter()
            .filter(|(v, _, _)| v == vxlan)
            .map(|(_, mac, dst)| (mac.clone(), dst.clone()))
            .collect())
    }

    async fn list_arp_entries(&self, vxlan: &str) -> Result<Vec<(String, String)>> {
        self.record(format!("list_arp_entries({vxlan})"));
        let entries = self.arp_entries.lock().expect("lock poisoned");
        Ok(entries
            .iter()
            .filter(|(v, _, _)| v == vxlan)
            .map(|(_, ip, mac)| (ip.clone(), mac.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_backend_records_calls() {
        let backend = MockBackend::new();
        let br = crate::naming::bridge_name("100");
        backend.create_bridge(&br).await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], format!("create_bridge({br})"));
    }

    #[tokio::test]
    async fn trait_method_coverage() {
        let b = MockBackend::new();
        let br100 = crate::naming::bridge_name("100");
        let br200 = crate::naming::bridge_name("200");
        let vx100 = crate::naming::vxlan_name("100");
        let tap = crate::naming::tap_name("vm1");
        let vh = crate::naming::veth_host_name("a");
        let vc = crate::naming::veth_container_name("b");

        b.create_vxlan(&vx100, 100, "fd00::1", 4789).await.unwrap();
        b.delete_vxlan(&vx100).await.unwrap();
        b.add_fdb_entry(&br100, "02:00:0a:01:01:03", "fd00::2")
            .await
            .unwrap();
        b.remove_fdb_entry(&br100, "02:00:0a:01:01:03")
            .await
            .unwrap();
        b.add_arp_proxy(&vx100, "10.1.1.3", "02:00:0a:01:01:03")
            .await
            .unwrap();
        b.remove_arp_proxy(&vx100, "10.1.1.3").await.unwrap();

        b.create_bridge(&br100).await.unwrap();
        b.add_bridge_ip(&br100, "10.1.1.1", 24).await.unwrap();
        b.remove_bridge_ip(&br100, "10.1.1.1").await.unwrap();
        b.delete_bridge(&br100).await.unwrap();
        b.attach_to_bridge(&vx100, &br100).await.unwrap();

        b.create_tap(&tap).await.unwrap();
        b.delete_tap(&tap).await.unwrap();
        b.create_veth_pair(&vh, &vc).await.unwrap();
        b.move_to_netns(&vc, 1234).await.unwrap();
        b.configure_netns(1234, &vc, "10.1.1.3", 24, "10.1.1.1", "02:00:0a:01:01:03")
            .await
            .unwrap();

        b.apply_vm_rules(&tap, "02:00:0a:01:01:03", "10.1.1.3")
            .await
            .unwrap();
        b.remove_vm_rules(&tap).await.unwrap();
        b.apply_nat(&br100, "10.1.1.0/24").await.unwrap();
        b.remove_nat(&br100, "10.1.1.0/24").await.unwrap();
        b.apply_peering_rules(&br100, &br200).await.unwrap();
        b.remove_peering_rules(&br100, &br200).await.unwrap();
        b.link_exists(&br100).await;
        b.list_interfaces(crate::naming::BRIDGE_PREFIX)
            .await
            .unwrap();
        b.list_fdb_entries(&vx100).await.unwrap();
        b.list_arp_entries(&vx100).await.unwrap();

        let calls = b.calls();
        assert_eq!(calls.len(), 26, "expected one call per trait method");

        // Verify each method was recorded
        assert!(calls[0].starts_with("create_vxlan("));
        assert!(calls[1].starts_with("delete_vxlan("));
        assert!(calls[2].starts_with("add_fdb_entry("));
        assert!(calls[3].starts_with("remove_fdb_entry("));
        assert!(calls[4].starts_with("add_arp_proxy("));
        assert!(calls[5].starts_with("remove_arp_proxy("));
        assert!(calls[6].starts_with("create_bridge("));
        assert!(calls[7].starts_with("add_bridge_ip("));
        assert!(calls[8].starts_with("remove_bridge_ip("));
        assert!(calls[9].starts_with("delete_bridge("));
        assert!(calls[10].starts_with("attach_to_bridge("));
        assert!(calls[11].starts_with("create_tap("));
        assert!(calls[12].starts_with("delete_tap("));
        assert!(calls[13].starts_with("create_veth_pair("));
        assert!(calls[14].starts_with("move_to_netns("));
        assert!(calls[15].starts_with("configure_netns("));
        assert!(calls[16].starts_with("apply_vm_rules("));
        assert!(calls[17].starts_with("remove_vm_rules("));
        assert!(calls[18].starts_with("apply_nat("));
        assert!(calls[19].starts_with("remove_nat("));
        assert!(calls[20].starts_with("apply_peering_rules("));
        assert!(calls[21].starts_with("remove_peering_rules("));
        assert!(calls[22].starts_with("link_exists("));
        assert!(calls[23].starts_with("list_interfaces("));

        // Test reset
        b.reset();
        assert!(b.calls().is_empty());
    }
}
