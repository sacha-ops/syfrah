use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use crate::backend::NetworkBackend;
use crate::error::Result;

/// Records every call for assertion in tests.
#[derive(Debug)]
pub struct MockBackend {
    /// Set of interface names that "exist" in the mock kernel.
    pub interfaces: Mutex<HashSet<String>>,
    /// Ordered log of calls made to this backend.
    pub calls: Mutex<Vec<String>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            interfaces: Mutex::new(HashSet::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn log(&self, msg: String) {
        self.calls.lock().unwrap().push(msg);
    }

    /// Return a snapshot of recorded calls.
    pub fn call_log(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Check if the mock knows about an interface.
    pub fn has_interface(&self, name: &str) -> bool {
        self.interfaces.lock().unwrap().contains(name)
    }

    /// Pre-register an interface so `interface_exists` returns true.
    pub fn add_existing_interface(&self, name: &str) {
        self.interfaces.lock().unwrap().insert(name.to_string());
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for MockBackend {
    // ── VXLAN ───────────────────────────────────────────────────────
    fn create_vxlan(&self, name: &str, vni: u32, local_ip: Ipv6Addr, port: u16) -> Result<()> {
        self.log(format!(
            "create_vxlan({name}, vni={vni}, local={local_ip}, port={port})"
        ));
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn delete_vxlan(&self, name: &str) -> Result<()> {
        self.log(format!("delete_vxlan({name})"));
        self.interfaces.lock().unwrap().remove(name);
        Ok(())
    }

    fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: Ipv6Addr) -> Result<()> {
        self.log(format!("add_fdb_entry({bridge}, {mac}, {vtep})"));
        Ok(())
    }

    fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()> {
        self.log(format!("remove_fdb_entry({bridge}, {mac})"));
        Ok(())
    }

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: &str) -> Result<()> {
        self.log(format!("add_arp_proxy({vxlan}, {ip}, {mac})"));
        Ok(())
    }

    // ── Bridge ──────────────────────────────────────────────────────
    fn create_bridge(&self, name: &str) -> Result<()> {
        self.log(format!("create_bridge({name})"));
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> Result<()> {
        self.log(format!("add_bridge_ip({bridge}, {gateway}/{prefix_len})"));
        Ok(())
    }

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<()> {
        self.log(format!("remove_bridge_ip({bridge}, {gateway})"));
        Ok(())
    }

    fn delete_bridge(&self, name: &str) -> Result<()> {
        self.log(format!("delete_bridge({name})"));
        self.interfaces.lock().unwrap().remove(name);
        Ok(())
    }

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()> {
        self.log(format!("attach_to_bridge({interface}, {bridge})"));
        Ok(())
    }

    // ── TAP / veth ──────────────────────────────────────────────────
    fn create_tap(&self, name: &str) -> Result<()> {
        self.log(format!("create_tap({name})"));
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn delete_tap(&self, name: &str) -> Result<()> {
        self.log(format!("delete_tap({name})"));
        self.interfaces.lock().unwrap().remove(name);
        Ok(())
    }

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()> {
        self.log(format!("create_veth_pair({name_a}, {name_b})"));
        self.interfaces.lock().unwrap().insert(name_a.to_string());
        self.interfaces.lock().unwrap().insert(name_b.to_string());
        Ok(())
    }

    fn delete_veth_pair(&self, name_a: &str) -> Result<()> {
        self.log(format!("delete_veth_pair({name_a})"));
        self.interfaces.lock().unwrap().remove(name_a);
        Ok(())
    }

    // ── Interface query ─────────────────────────────────────────────
    fn interface_exists(&self, name: &str) -> Result<bool> {
        Ok(self.interfaces.lock().unwrap().contains(name))
    }

    // ── Firewall ────────────────────────────────────────────────────
    fn apply_vm_rules(&self, tap: &str, mac: &str, ip: Ipv4Addr) -> Result<()> {
        self.log(format!("apply_vm_rules({tap}, {mac}, {ip})"));
        Ok(())
    }

    fn remove_vm_rules(&self, tap: &str) -> Result<()> {
        self.log(format!("remove_vm_rules({tap})"));
        Ok(())
    }

    fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()> {
        self.log(format!("apply_nat({bridge}, {subnet_cidr})"));
        Ok(())
    }

    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        self.log(format!("apply_peering_rules({bridge_a}, {bridge_b})"));
        Ok(())
    }
}
