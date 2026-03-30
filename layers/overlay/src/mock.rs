//! Mock network backend for unit tests.
//!
//! Records every call so tests can assert the exact sequence and arguments
//! of networking operations without requiring root or real interfaces.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use ipnet::Ipv4Net;

use crate::backend::{MacAddr, NetworkBackend};
use crate::error::OverlayError;

/// A recorded call to the mock backend.
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
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
        mac: MacAddr,
        vtep: Ipv6Addr,
    },
    RemoveFdbEntry {
        bridge: String,
        mac: MacAddr,
    },
    AddArpProxy {
        vxlan: String,
        ip: Ipv4Addr,
        mac: MacAddr,
    },
    RemoveArpProxy {
        vxlan: String,
        ip: Ipv4Addr,
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
        mac: MacAddr,
        ip: Ipv4Addr,
    },
    RemoveVmRules {
        tap: String,
    },
    ApplyNat {
        bridge: String,
        subnet: Ipv4Net,
    },
    ApplyPeeringRules {
        bridge_a: String,
        bridge_b: String,
    },
}

/// Mock implementation of `NetworkBackend` that records all calls.
pub struct MockBackend {
    calls: Mutex<Vec<MockCall>>,
}

impl MockBackend {
    /// Create a new empty mock backend.
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Clear recorded calls.
    pub fn clear(&self) {
        self.calls.lock().unwrap().clear();
    }

    fn record(&self, call: MockCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for MockBackend {
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), OverlayError> {
        self.record(MockCall::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        });
        Ok(())
    }

    fn delete_vxlan(&self, name: &str) -> Result<(), OverlayError> {
        self.record(MockCall::DeleteVxlan {
            name: name.to_string(),
        });
        Ok(())
    }

    fn add_fdb_entry(
        &self,
        bridge: &str,
        mac: MacAddr,
        vtep: Ipv6Addr,
    ) -> Result<(), OverlayError> {
        self.record(MockCall::AddFdbEntry {
            bridge: bridge.to_string(),
            mac,
            vtep,
        });
        Ok(())
    }

    fn remove_fdb_entry(&self, bridge: &str, mac: MacAddr) -> Result<(), OverlayError> {
        self.record(MockCall::RemoveFdbEntry {
            bridge: bridge.to_string(),
            mac,
        });
        Ok(())
    }

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: MacAddr) -> Result<(), OverlayError> {
        self.record(MockCall::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac,
        });
        Ok(())
    }

    fn remove_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr) -> Result<(), OverlayError> {
        self.record(MockCall::RemoveArpProxy {
            vxlan: vxlan.to_string(),
            ip,
        });
        Ok(())
    }

    fn create_bridge(&self, name: &str) -> Result<(), OverlayError> {
        self.record(MockCall::CreateBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), OverlayError> {
        self.record(MockCall::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        });
        Ok(())
    }

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), OverlayError> {
        self.record(MockCall::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        });
        Ok(())
    }

    fn delete_bridge(&self, name: &str) -> Result<(), OverlayError> {
        self.record(MockCall::DeleteBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), OverlayError> {
        self.record(MockCall::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        });
        Ok(())
    }

    fn create_tap(&self, name: &str) -> Result<(), OverlayError> {
        self.record(MockCall::CreateTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn delete_tap(&self, name: &str) -> Result<(), OverlayError> {
        self.record(MockCall::DeleteTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), OverlayError> {
        self.record(MockCall::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        });
        Ok(())
    }

    fn apply_vm_rules(&self, tap: &str, mac: MacAddr, ip: Ipv4Addr) -> Result<(), OverlayError> {
        self.record(MockCall::ApplyVmRules {
            tap: tap.to_string(),
            mac,
            ip,
        });
        Ok(())
    }

    fn remove_vm_rules(&self, tap: &str) -> Result<(), OverlayError> {
        self.record(MockCall::RemoveVmRules {
            tap: tap.to_string(),
        });
        Ok(())
    }

    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), OverlayError> {
        self.record(MockCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet,
        });
        Ok(())
    }

    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), OverlayError> {
        self.record(MockCall::ApplyPeeringRules {
            bridge_a: bridge_a.to_string(),
            bridge_b: bridge_b.to_string(),
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_backend_records_calls() {
        let backend = MockBackend::new();
        let ip: Ipv6Addr = "fd12:3456:7800::1".parse().unwrap();

        backend.create_vxlan("syfvx-100", 100, ip, 4789).unwrap();
        backend.create_bridge("syfbr-100").unwrap();

        let calls = backend.calls();
        assert_eq!(calls.len(), 2);
        assert!(matches!(&calls[0], MockCall::CreateVxlan { name, vni, .. }
            if name == "syfvx-100" && *vni == 100));
        assert!(matches!(&calls[1], MockCall::CreateBridge { name }
            if name == "syfbr-100"));
    }

    #[test]
    fn trait_method_coverage() {
        // Verify every trait method is callable on the mock.
        let backend = MockBackend::new();
        let ipv6: Ipv6Addr = "fd12::1".parse().unwrap();
        let ipv4: Ipv4Addr = "10.0.1.5".parse().unwrap();
        let mac = MacAddr([0x02, 0x00, 0x0a, 0x00, 0x01, 0x05]);
        let subnet: Ipv4Net = "10.0.1.0/24".parse().unwrap();

        backend.create_vxlan("vx", 100, ipv6, 4789).unwrap();
        backend.delete_vxlan("vx").unwrap();
        backend.add_fdb_entry("br", mac, ipv6).unwrap();
        backend.remove_fdb_entry("br", mac).unwrap();
        backend.add_arp_proxy("vx", ipv4, mac).unwrap();
        backend.remove_arp_proxy("vx", ipv4).unwrap();
        backend.create_bridge("br").unwrap();
        backend.add_bridge_ip("br", ipv4, 24).unwrap();
        backend.remove_bridge_ip("br", ipv4).unwrap();
        backend.delete_bridge("br").unwrap();
        backend.attach_to_bridge("tap0", "br").unwrap();
        backend.create_tap("tap0").unwrap();
        backend.delete_tap("tap0").unwrap();
        backend.create_veth_pair("a", "b").unwrap();
        backend.apply_vm_rules("tap0", mac, ipv4).unwrap();
        backend.remove_vm_rules("tap0").unwrap();
        backend.apply_nat("br", subnet).unwrap();
        backend.apply_peering_rules("br-a", "br-b").unwrap();

        assert_eq!(backend.calls().len(), 18);
    }
}
