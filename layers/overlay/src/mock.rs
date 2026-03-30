use std::collections::HashSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use crate::backend::{BackendError, BackendResult, NetworkBackend};

/// A recorded call to the mock backend.
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
    InterfaceExists {
        name: String,
    },
    AttachToBridge {
        interface: String,
        bridge: String,
    },
    AddFdbEntry {
        dev: String,
        mac: String,
        vtep: Ipv6Addr,
    },
    RemoveFdbEntry {
        dev: String,
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

/// Mock network backend that records all calls for assertion in tests.
pub struct MockBackend {
    calls: Mutex<Vec<MockCall>>,
    /// Interfaces that "exist" in the mock.
    interfaces: Mutex<HashSet<String>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            interfaces: Mutex::new(HashSet::new()),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Pre-register an interface as existing.
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
    fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        });
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn delete_vxlan(&self, name: &str) -> BackendResult<()> {
        let mut ifaces = self.interfaces.lock().unwrap();
        if !ifaces.remove(name) {
            return Err(BackendError::InterfaceNotFound(name.to_string()));
        }
        self.calls.lock().unwrap().push(MockCall::DeleteVxlan {
            name: name.to_string(),
        });
        Ok(())
    }

    fn interface_exists(&self, name: &str) -> BackendResult<bool> {
        self.calls.lock().unwrap().push(MockCall::InterfaceExists {
            name: name.to_string(),
        });
        Ok(self.interfaces.lock().unwrap().contains(name))
    }

    fn attach_to_bridge(&self, interface: &str, bridge: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        });
        Ok(())
    }

    fn add_fdb_entry(&self, dev: &str, mac: &str, vtep: Ipv6Addr) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::AddFdbEntry {
            dev: dev.to_string(),
            mac: mac.to_string(),
            vtep,
        });
        Ok(())
    }

    fn remove_fdb_entry(&self, dev: &str, mac: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::RemoveFdbEntry {
            dev: dev.to_string(),
            mac: mac.to_string(),
        });
        Ok(())
    }

    fn add_arp_proxy(&self, vxlan: &str, ip: Ipv4Addr, mac: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac: mac.to_string(),
        });
        Ok(())
    }

    fn create_bridge(&self, name: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::CreateBridge {
            name: name.to_string(),
        });
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn add_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr, prefix_len: u8) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        });
        Ok(())
    }

    fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        });
        Ok(())
    }

    fn delete_bridge(&self, name: &str) -> BackendResult<()> {
        self.interfaces.lock().unwrap().remove(name);
        self.calls.lock().unwrap().push(MockCall::DeleteBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn create_tap(&self, name: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::CreateTap {
            name: name.to_string(),
        });
        self.interfaces.lock().unwrap().insert(name.to_string());
        Ok(())
    }

    fn delete_tap(&self, name: &str) -> BackendResult<()> {
        self.interfaces.lock().unwrap().remove(name);
        self.calls.lock().unwrap().push(MockCall::DeleteTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn create_veth_pair(&self, name_a: &str, name_b: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        });
        self.interfaces.lock().unwrap().insert(name_a.to_string());
        self.interfaces.lock().unwrap().insert(name_b.to_string());
        Ok(())
    }

    fn apply_vm_rules(&self, tap: &str, mac: &str, ip: Ipv4Addr) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::ApplyVmRules {
            tap: tap.to_string(),
            mac: mac.to_string(),
            ip,
        });
        Ok(())
    }

    fn remove_vm_rules(&self, tap: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::RemoveVmRules {
            tap: tap.to_string(),
        });
        Ok(())
    }

    fn apply_nat(&self, bridge: &str, subnet_cidr: &str) -> BackendResult<()> {
        self.calls.lock().unwrap().push(MockCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet_cidr: subnet_cidr.to_string(),
        });
        Ok(())
    }

    fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> BackendResult<()> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::ApplyPeeringRules {
                bridge_a: bridge_a.to_string(),
                bridge_b: bridge_b.to_string(),
            });
        Ok(())
    }
}
