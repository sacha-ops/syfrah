use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Mutex;

use crate::backend::{BackendError, NetworkBackend};

/// Records every call for assertion in tests.
#[derive(Debug)]
pub struct MockBackend {
    pub calls: Mutex<Vec<MockCall>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
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
        mac: [u8; 6],
        vtep: Ipv6Addr,
    },
    RemoveFdbEntry {
        bridge: String,
        mac: [u8; 6],
    },
    AddArpProxy {
        vxlan: String,
        ip: Ipv4Addr,
        mac: [u8; 6],
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
        mac: [u8; 6],
        ip: Ipv4Addr,
    },
    RemoveVmRules {
        tap: String,
    },
    ApplyNat {
        bridge: String,
        subnet: Ipv4Addr,
        prefix_len: u8,
    },
    ApplyPeeringRules {
        bridge_a: String,
        bridge_b: String,
    },
}

impl MockBackend {
    pub fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NetworkBackend for MockBackend {
    async fn create_vxlan(
        &self,
        name: &str,
        vni: u32,
        local_ip: Ipv6Addr,
        port: u16,
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::CreateVxlan {
            name: name.to_string(),
            vni,
            local_ip,
            port,
        });
        Ok(())
    }

    async fn delete_vxlan(&self, name: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::DeleteVxlan {
            name: name.to_string(),
        });
        Ok(())
    }

    async fn add_fdb_entry(
        &self,
        bridge: &str,
        mac: [u8; 6],
        vtep: Ipv6Addr,
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::AddFdbEntry {
            bridge: bridge.to_string(),
            mac,
            vtep,
        });
        Ok(())
    }

    async fn remove_fdb_entry(&self, bridge: &str, mac: [u8; 6]) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::RemoveFdbEntry {
            bridge: bridge.to_string(),
            mac,
        });
        Ok(())
    }

    async fn add_arp_proxy(
        &self,
        vxlan: &str,
        ip: Ipv4Addr,
        mac: [u8; 6],
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::AddArpProxy {
            vxlan: vxlan.to_string(),
            ip,
            mac,
        });
        Ok(())
    }

    async fn create_bridge(&self, name: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::CreateBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    async fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::AddBridgeIp {
            bridge: bridge.to_string(),
            gateway,
            prefix_len,
        });
        Ok(())
    }

    async fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::RemoveBridgeIp {
            bridge: bridge.to_string(),
            gateway,
        });
        Ok(())
    }

    async fn delete_bridge(&self, name: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::DeleteBridge {
            name: name.to_string(),
        });
        Ok(())
    }

    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::AttachToBridge {
            interface: interface.to_string(),
            bridge: bridge.to_string(),
        });
        Ok(())
    }

    async fn create_tap(&self, name: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::CreateTap {
            name: name.to_string(),
        });
        Ok(())
    }

    async fn delete_tap(&self, name: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::DeleteTap {
            name: name.to_string(),
        });
        Ok(())
    }

    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::CreateVethPair {
            name_a: name_a.to_string(),
            name_b: name_b.to_string(),
        });
        Ok(())
    }

    async fn apply_vm_rules(
        &self,
        tap: &str,
        mac: [u8; 6],
        ip: Ipv4Addr,
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::ApplyVmRules {
            tap: tap.to_string(),
            mac,
            ip,
        });
        Ok(())
    }

    async fn remove_vm_rules(&self, tap: &str) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::RemoveVmRules {
            tap: tap.to_string(),
        });
        Ok(())
    }

    async fn apply_nat(
        &self,
        bridge: &str,
        subnet: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError> {
        self.calls.lock().unwrap().push(MockCall::ApplyNat {
            bridge: bridge.to_string(),
            subnet,
            prefix_len,
        });
        Ok(())
    }

    async fn apply_peering_rules(
        &self,
        bridge_a: &str,
        bridge_b: &str,
    ) -> Result<(), BackendError> {
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
