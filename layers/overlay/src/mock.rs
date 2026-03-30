use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use crate::backend::{Ipv4Net, MacAddr, NetworkBackend};

/// A recorded call made to the mock backend.
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
    ApplySubnetIsolation {
        bridge: String,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    },
    RemoveSubnetIsolation {
        bridge: String,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    },
    ApplyVpcIsolation {
        bridge_a: String,
        bridge_b: String,
    },
    RemoveVpcIsolation {
        bridge_a: String,
        bridge_b: String,
    },
}

/// Mock implementation of [`NetworkBackend`] that records all calls.
pub struct MockBackend {
    pub calls: Mutex<Vec<MockCall>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded calls.
    pub fn recorded_calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Return `true` if the given call appears in the recorded list.
    pub fn has_call(&self, call: &MockCall) -> bool {
        self.calls.lock().unwrap().contains(call)
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
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        });
        Ok(())
    }

    fn delete_vxlan(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::DeleteVxlan {
            name: name.to_string(),
        });
        Ok(())
    }

    fn add_fdb_entry(
        &self,
        bridge: &str,
        mac: MacAddr,
        vtep: Ipv6Addr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::AddFdbEntry {
            bridge: bridge.to_string(),
            mac,
            vtep,
        });
        Ok(())
    }

    fn remove_fdb_entry(
        &self,
        bridge: &str,
        mac: MacAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::RemoveFdbEntry {
            bridge: bridge.to_string(),
            mac,
        });
        Ok(())
    }

    fn add_arp_proxy(
        &self,
        vxlan: &str,
        ip: Ipv4Addr,
        mac: MacAddr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac,
        });
        Ok(())
    }

    fn create_bridge(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::CreateBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        });
        Ok(())
    }

    fn remove_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        });
        Ok(())
    }

    fn delete_bridge(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::DeleteBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    fn attach_to_bridge(
        &self,
        interface: &str,
        bridge: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        });
        Ok(())
    }

    fn create_tap(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::CreateTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn delete_tap(&self, name: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::DeleteTap {
            name: name.to_string(),
        });
        Ok(())
    }

    fn create_veth_pair(
        &self,
        name_a: &str,
        name_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        });
        Ok(())
    }

    fn apply_vm_rules(
        &self,
        tap: &str,
        mac: MacAddr,
        ip: Ipv4Addr,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::ApplyVmRules {
            tap: tap.to_string(),
            mac,
            ip,
        });
        Ok(())
    }

    fn remove_vm_rules(&self, tap: &str) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::RemoveVmRules {
            tap: tap.to_string(),
        });
        Ok(())
    }

    fn apply_nat(&self, bridge: &str, subnet: Ipv4Net) -> Result<(), Box<dyn std::error::Error>> {
        self.calls.lock().unwrap().push(MockCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet,
        });
        Ok(())
    }

    fn apply_peering_rules(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::ApplyPeeringRules {
                bridge_a: bridge_a.to_string(),
                bridge_b: bridge_b.to_string(),
            });
        Ok(())
    }

    fn apply_subnet_isolation(
        &self,
        bridge: &str,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::ApplySubnetIsolation {
                bridge: bridge.to_string(),
                subnet_a,
                subnet_b,
            });
        Ok(())
    }

    fn remove_subnet_isolation(
        &self,
        bridge: &str,
        subnet_a: Ipv4Net,
        subnet_b: Ipv4Net,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::RemoveSubnetIsolation {
                bridge: bridge.to_string(),
                subnet_a,
                subnet_b,
            });
        Ok(())
    }

    fn apply_vpc_isolation(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::ApplyVpcIsolation {
                bridge_a: bridge_a.to_string(),
                bridge_b: bridge_b.to_string(),
            });
        Ok(())
    }

    fn remove_vpc_isolation(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.calls
            .lock()
            .unwrap()
            .push(MockCall::RemoveVpcIsolation {
                bridge_a: bridge_a.to_string(),
                bridge_b: bridge_b.to_string(),
            });
        Ok(())
    }
}
