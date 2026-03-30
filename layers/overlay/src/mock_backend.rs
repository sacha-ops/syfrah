//! Mock implementation of `NetworkBackend` for unit tests.
//!
//! Records every call so tests can assert the exact sequence
//! and content of networking operations.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use crate::backend::{BackendError, NetworkBackend, Result};

/// A recorded call to the network backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    CreateVxlan {
        name: String,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    },
    DeleteVxlan {
        name: String,
    },
    AddFdbEntry {
        bridge: String,
        mac: String,
        vtep: Ipv6Addr,
    },
    RemoveFdbEntry {
        bridge: String,
        mac: String,
    },
    AddArpProxy {
        vxlan: String,
        ip: Ipv4Addr,
        mac: String,
    },
    CreateBridge {
        name: String,
    },
    AddBridgeIp {
        bridge: String,
        gateway: Ipv4Addr,
        prefix_len: u8,
    },
    RemoveBridgeIp {
        bridge: String,
        gateway: Ipv4Addr,
    },
    DeleteBridge {
        name: String,
    },
    AttachToBridge {
        interface: String,
        bridge: String,
    },
    CreateTap {
        name: String,
    },
    DeleteTap {
        name: String,
    },
    CreateVethPair {
        name_a: String,
        name_b: String,
    },
    ApplyVmRules {
        tap: String,
        mac: String,
        ip: Ipv4Addr,
    },
    RemoveVmRules {
        tap: String,
    },
    ApplyNat {
        bridge: String,
        subnet_cidr: String,
    },
    ApplyPeeringRules {
        bridge_a: String,
        bridge_b: String,
    },
}

/// Mock backend that records all calls.
pub struct MockBackend {
    calls: Mutex<Vec<Call>>,
    /// When set, every call returns this error.
    fail_with: Mutex<Option<String>>,
}

impl MockBackend {
    /// Create a new mock backend.
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail_with: Mutex::new(None),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Make all subsequent calls fail with the given message.
    pub fn set_fail(&self, msg: &str) {
        *self.fail_with.lock().unwrap() = Some(msg.to_string());
    }

    fn record(&self, call: Call) -> Result<()> {
        if let Some(msg) = self.fail_with.lock().unwrap().as_ref() {
            return Err(BackendError::CommandFailed(msg.clone()));
        }
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for MockBackend {
    fn create_vxlan(&self, name: &str, vni: u32, local_ip: Ipv6Addr, port: u16) -> Result<()> {
        self.record(Call::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        })
    }

    fn delete_vxlan(&self, name: &str) -> Result<()> {
        self.record(Call::DeleteVxlan {
            name: name.to_string(),
        })
    }

    fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: Ipv6Addr) -> Result<()> {
        self.record(Call::AddFdbEntry {
            bridge: bridge.to_string(),
            mac: mac.to_string(),
            vtep,
        })
    }

    fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()> {
        self.record(Call::RemoveFdbEntry {
            bridge: bridge.to_string(),
            mac: mac.to_string(),
        })
    }

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: &str) -> Result<()> {
        self.record(Call::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac: mac.to_string(),
        })
    }

    fn create_bridge(&self, name: &str) -> Result<()> {
        self.record(Call::CreateBridge {
            name: name.to_string(),
        })
    }

    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> Result<()> {
        self.record(Call::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        })
    }

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<()> {
        self.record(Call::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        })
    }

    fn delete_bridge(&self, name: &str) -> Result<()> {
        self.record(Call::DeleteBridge {
            name: name.to_string(),
        })
    }

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()> {
        self.record(Call::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        })
    }

    fn create_tap(&self, name: &str) -> Result<()> {
        self.record(Call::CreateTap {
            name: name.to_string(),
        })
    }

    fn delete_tap(&self, name: &str) -> Result<()> {
        self.record(Call::DeleteTap {
            name: name.to_string(),
        })
    }

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()> {
        self.record(Call::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        })
    }

    fn apply_vm_rules(&self, tap: &str, mac: &str, ip: Ipv4Addr) -> Result<()> {
        self.record(Call::ApplyVmRules {
            tap: tap.to_string(),
            mac: mac.to_string(),
            ip,
        })
    }

    fn remove_vm_rules(&self, tap: &str) -> Result<()> {
        self.record(Call::RemoveVmRules {
            tap: tap.to_string(),
        })
    }

    fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> Result<()> {
        self.record(Call::ApplyNat {
            bridge: bridge.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
        })
    }

    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        self.record(Call::ApplyPeeringRules {
            bridge_a: bridge_a.to_string(),
            bridge_b: bridge_b.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_backend_records_calls() {
        let backend = MockBackend::new();
        backend.create_bridge("syfbr-100").unwrap();
        backend.create_tap("syftap-vm1").unwrap();
        backend.attach_to_bridge("syftap-vm1", "syfbr-100").unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 3);
        assert_eq!(
            calls[0],
            Call::CreateBridge {
                name: "syfbr-100".to_string()
            }
        );
        assert_eq!(
            calls[1],
            Call::CreateTap {
                name: "syftap-vm1".to_string()
            }
        );
        assert_eq!(
            calls[2],
            Call::AttachToBridge {
                interface: "syftap-vm1".to_string(),
                bridge: "syfbr-100".to_string()
            }
        );
    }

    #[test]
    fn trait_method_coverage() {
        let backend = MockBackend::new();
        let ipv6 = "fd00::1".parse().unwrap();
        let ipv4: Ipv4Addr = "10.0.1.5".parse().unwrap();

        // Exercise every trait method to ensure the mock covers them all.
        backend.create_vxlan("syfvx-100", 100, ipv6, 4789).unwrap();
        backend.delete_vxlan("syfvx-100").unwrap();
        backend
            .add_fdb_entry("syfbr-100", "02:00:0a:00:01:05", ipv6)
            .unwrap();
        backend
            .remove_fdb_entry("syfbr-100", "02:00:0a:00:01:05")
            .unwrap();
        backend
            .add_arp_proxy("syfvx-100", ipv4, "02:00:0a:00:01:05")
            .unwrap();
        backend.create_bridge("syfbr-100").unwrap();
        backend.add_bridge_ip("syfbr-100", ipv4, 24).unwrap();
        backend.remove_bridge_ip("syfbr-100", ipv4).unwrap();
        backend.delete_bridge("syfbr-100").unwrap();
        backend.attach_to_bridge("syftap-vm1", "syfbr-100").unwrap();
        backend.create_tap("syftap-vm1").unwrap();
        backend.delete_tap("syftap-vm1").unwrap();
        backend.create_veth_pair("syfpeer-a", "syfpeer-b").unwrap();
        backend
            .apply_vm_rules("syftap-vm1", "02:00:0a:00:01:05", ipv4)
            .unwrap();
        backend.remove_vm_rules("syftap-vm1").unwrap();
        backend.apply_nat("syfbr-100", "10.0.1.0/24").unwrap();
        backend
            .apply_peering_rules("syfbr-100", "syfbr-200")
            .unwrap();

        // 17 trait methods = 17 calls
        assert_eq!(backend.calls().len(), 17);
    }
}
