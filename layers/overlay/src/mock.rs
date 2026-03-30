use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use ipnet::Ipv4Net;

use crate::backend::{BackendError, MacAddr, NetworkBackend};

/// A recorded call to the [`MockNetworkBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
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
    RemovePeeringRules {
        bridge_a: String,
        bridge_b: String,
    },
}

/// Mock backend that records every call for assertion in tests.
///
/// Thread-safe via interior mutability so it can be shared across async tasks.
pub struct MockNetworkBackend {
    calls: Mutex<Vec<MockCall>>,
}

impl MockNetworkBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Return only the calls that match a given predicate.
    pub fn calls_matching<F: Fn(&MockCall) -> bool>(&self, f: F) -> Vec<MockCall> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| f(c))
            .cloned()
            .collect()
    }

    fn record(&self, call: MockCall) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Default for MockNetworkBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for MockNetworkBackend {
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), BackendError> {
        self.record(MockCall::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        });
        Ok(())
    }

    fn delete_vxlan(&self, name: &str) -> Result<(), BackendError> {
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
    ) -> Result<(), BackendError> {
        self.record(MockCall::AddFdbEntry {
            bridge: bridge.to_string(),
            mac,
            vtep,
        });
        Ok(())
    }

    fn remove_fdb_entry(&self, bridge: &str, mac: MacAddr) -> Result<(), BackendError> {
        self.record(MockCall::RemoveFdbEntry {
            bridge: bridge.to_string(),
            mac,
        });
        Ok(())
    }

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: MacAddr) -> Result<(), BackendError> {
        self.record(MockCall::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac,
        });
        Ok(())
    }

    fn create_bridge(&self, name: &str) -> Result<(), BackendError> {
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
    ) -> Result<(), BackendError> {
        self.record(MockCall::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        });
        Ok(())
    }

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), BackendError> {
        self.record(MockCall::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        });
        Ok(())
    }

    fn delete_bridge(&self, name: &str) -> Result<(), BackendError> {
        self.record(MockCall::DeleteBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), BackendError> {
        self.record(MockCall::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        });
        Ok(())
    }

    fn create_tap(&self, name: &str) -> Result<(), BackendError> {
        self.record(MockCall::CreateTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn delete_tap(&self, name: &str) -> Result<(), BackendError> {
        self.record(MockCall::DeleteTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), BackendError> {
        self.record(MockCall::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        });
        Ok(())
    }

    fn apply_vm_rules(&self, tap: &str, mac: MacAddr, ip: Ipv4Addr) -> Result<(), BackendError> {
        self.record(MockCall::ApplyVmRules {
            tap: tap.to_string(),
            mac,
            ip,
        });
        Ok(())
    }

    fn remove_vm_rules(&self, tap: &str) -> Result<(), BackendError> {
        self.record(MockCall::RemoveVmRules {
            tap: tap.to_string(),
        });
        Ok(())
    }

    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), BackendError> {
        self.record(MockCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet,
        });
        Ok(())
    }

    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), BackendError> {
        self.record(MockCall::ApplyPeeringRules {
            bridge_a: bridge_a.to_string(),
            bridge_b: bridge_b.to_string(),
        });
        Ok(())
    }

    fn remove_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<(), BackendError> {
        self.record(MockCall::RemovePeeringRules {
            bridge_a: bridge_a.to_string(),
            bridge_b: bridge_b.to_string(),
        });
        Ok(())
    }
}
