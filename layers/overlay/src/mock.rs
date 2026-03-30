use std::sync::Mutex;

use crate::backend::NetworkBackend;
use crate::error::Result;

/// In-memory mock that records every call for test assertions.
///
/// `existing_links` can be pre-populated to control `link_exists` responses.
pub struct MockBackend {
    calls: Mutex<Vec<String>>,
    existing_links: Mutex<std::collections::HashSet<String>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            existing_links: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Register an interface name so `link_exists` returns `true` for it.
    pub fn add_existing_link(&self, name: &str) {
        self.existing_links
            .lock()
            .expect("lock poisoned")
            .insert(name.to_string());
    }

    /// Remove an interface name so `link_exists` returns `false` for it.
    pub fn remove_existing_link(&self, name: &str) {
        self.existing_links
            .lock()
            .expect("lock poisoned")
            .remove(name);
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock poisoned").clone()
    }

    /// Clear recorded calls.
    pub fn reset(&self) {
        self.calls.lock().expect("lock poisoned").clear();
    }

    fn record(&self, call: String) {
        self.calls.lock().expect("lock poisoned").push(call);
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
        self.record(format!("add_fdb_entry({bridge}, {mac}, {vtep})"));
        Ok(())
    }

    async fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()> {
        self.record(format!("remove_fdb_entry({bridge}, {mac})"));
        Ok(())
    }

    async fn add_arp_proxy(&self, vxlan: &str, ip: &str, mac: &str) -> Result<()> {
        self.record(format!("add_arp_proxy({vxlan}, {ip}, {mac})"));
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
        self.record(format!("create_tap({name})"));
        Ok(())
    }

    async fn delete_tap(&self, name: &str) -> Result<()> {
        self.record(format!("delete_tap({name})"));
        Ok(())
    }

    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()> {
        self.record(format!("create_veth_pair({name_a}, {name_b})"));
        self.existing_links
            .lock()
            .expect("lock poisoned")
            .insert(name_a.to_string());
        self.existing_links
            .lock()
            .expect("lock poisoned")
            .insert(name_b.to_string());
        Ok(())
    }

    async fn delete_veth_pair(&self, name: &str) -> Result<()> {
        self.record(format!("delete_veth_pair({name})"));
        self.existing_links
            .lock()
            .expect("lock poisoned")
            .remove(name);
        Ok(())
    }

    async fn set_link_up(&self, name: &str) -> Result<()> {
        self.record(format!("set_link_up({name})"));
        Ok(())
    }

    async fn link_exists(&self, name: &str) -> Result<bool> {
        self.record(format!("link_exists({name})"));
        Ok(self
            .existing_links
            .lock()
            .expect("lock poisoned")
            .contains(name))
    }

    // ── Routing ────────────────────────────────────────────────────────

    async fn add_route(&self, cidr: &str, dev: &str) -> Result<()> {
        self.record(format!("add_route({cidr}, {dev})"));
        Ok(())
    }

    async fn delete_route(&self, cidr: &str, dev: &str) -> Result<()> {
        self.record(format!("delete_route({cidr}, {dev})"));
        Ok(())
    }

    // ── Firewall ───────────────────────────────────────────────────────

    async fn apply_vm_rules(&self, tap: &str, mac: &str, ip: &str) -> Result<()> {
        self.record(format!("apply_vm_rules({tap}, {mac}, {ip})"));
        Ok(())
    }

    async fn remove_vm_rules(&self, tap: &str) -> Result<()> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_backend_records_calls() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-100").await.unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0], "create_bridge(syfbr-100)");
    }

    #[tokio::test]
    async fn trait_method_coverage() {
        let b = MockBackend::new();

        b.create_vxlan("syfvx-100", 100, "fd00::1", 4789)
            .await
            .unwrap();
        b.delete_vxlan("syfvx-100").await.unwrap();
        b.add_fdb_entry("syfbr-100", "02:00:0a:01:01:03", "fd00::2")
            .await
            .unwrap();
        b.remove_fdb_entry("syfbr-100", "02:00:0a:01:01:03")
            .await
            .unwrap();
        b.add_arp_proxy("syfvx-100", "10.1.1.3", "02:00:0a:01:01:03")
            .await
            .unwrap();

        b.create_bridge("syfbr-100").await.unwrap();
        b.add_bridge_ip("syfbr-100", "10.1.1.1", 24).await.unwrap();
        b.remove_bridge_ip("syfbr-100", "10.1.1.1").await.unwrap();
        b.delete_bridge("syfbr-100").await.unwrap();
        b.attach_to_bridge("syfvx-100", "syfbr-100").await.unwrap();

        b.create_tap("syftap-vm1").await.unwrap();
        b.delete_tap("syftap-vm1").await.unwrap();
        b.create_veth_pair("syfve-a", "syfve-b").await.unwrap();
        b.delete_veth_pair("syfve-a").await.unwrap();
        b.set_link_up("syfve-b").await.unwrap();
        let _exists = b.link_exists("syfve-b").await.unwrap();
        b.add_route("10.2.0.0/16", "syfve-a").await.unwrap();
        b.delete_route("10.2.0.0/16", "syfve-a").await.unwrap();

        b.apply_vm_rules("syftap-vm1", "02:00:0a:01:01:03", "10.1.1.3")
            .await
            .unwrap();
        b.remove_vm_rules("syftap-vm1").await.unwrap();
        b.apply_nat("syfbr-100", "10.1.1.0/24").await.unwrap();
        b.remove_nat("syfbr-100", "10.1.1.0/24").await.unwrap();
        b.apply_peering_rules("syfbr-100", "syfbr-200")
            .await
            .unwrap();
        b.remove_peering_rules("syfbr-100", "syfbr-200")
            .await
            .unwrap();

        let calls = b.calls();
        assert_eq!(calls.len(), 24, "expected one call per trait method");

        // Verify each method was recorded
        assert!(calls[0].starts_with("create_vxlan("));
        assert!(calls[1].starts_with("delete_vxlan("));
        assert!(calls[2].starts_with("add_fdb_entry("));
        assert!(calls[3].starts_with("remove_fdb_entry("));
        assert!(calls[4].starts_with("add_arp_proxy("));
        assert!(calls[5].starts_with("create_bridge("));
        assert!(calls[6].starts_with("add_bridge_ip("));
        assert!(calls[7].starts_with("remove_bridge_ip("));
        assert!(calls[8].starts_with("delete_bridge("));
        assert!(calls[9].starts_with("attach_to_bridge("));
        assert!(calls[10].starts_with("create_tap("));
        assert!(calls[11].starts_with("delete_tap("));
        assert!(calls[12].starts_with("create_veth_pair("));
        assert!(calls[13].starts_with("delete_veth_pair("));
        assert!(calls[14].starts_with("set_link_up("));
        assert!(calls[15].starts_with("link_exists("));
        assert!(calls[16].starts_with("add_route("));
        assert!(calls[17].starts_with("delete_route("));
        assert!(calls[18].starts_with("apply_vm_rules("));
        assert!(calls[19].starts_with("remove_vm_rules("));
        assert!(calls[20].starts_with("apply_nat("));
        assert!(calls[21].starts_with("remove_nat("));
        assert!(calls[22].starts_with("apply_peering_rules("));
        assert!(calls[23].starts_with("remove_peering_rules("));

        // Test reset
        b.reset();
        assert!(b.calls().is_empty());
    }
}
