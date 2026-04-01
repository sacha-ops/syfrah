//! VM placement announcements for FDB distribution.
//!
//! When a VM is created or deleted, all nodes in the VPC must update their FDB
//! and ARP proxy tables. This module provides the announcement type and
//! serialization for transport over the fabric announcement channel.
//!
//! On single node: announcements are stored locally but no remote distribution
//! occurs.
//!
//! On multi-node: each node receives the announcement and updates its FDB and
//! ARP proxy entries accordingly.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// The action to perform on a VM placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlacementAction {
    Add,
    Remove,
}

impl std::fmt::Display for PlacementAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementAction::Add => write!(f, "add"),
            PlacementAction::Remove => write!(f, "remove"),
        }
    }
}

/// A VM placement announcement broadcast to all fabric peers.
///
/// This is the message sent when a VM is created or deleted, so that all nodes
/// in the VPC can update their FDB and ARP proxy tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmPlacementAnnouncement {
    /// The VPC that owns this VM.
    pub vpc_id: String,
    /// The unique identifier of the VM.
    pub vm_id: String,
    /// The MAC address assigned to the VM (e.g. `02:00:0a:00:01:05`).
    pub vm_mac: String,
    /// The IP address assigned to the VM (e.g. `10.0.1.5`).
    pub vm_ip: String,
    /// The subnet the VM belongs to.
    pub subnet_id: String,
    /// The fabric address of the node hosting this VM (e.g. the node's
    /// WireGuard IPv6 address).
    pub hypervisor_id: String,
    /// Whether this is an add or remove announcement.
    pub action: PlacementAction,
}

impl VmPlacementAnnouncement {
    /// Serialize this announcement to JSON bytes for transport.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize an announcement from JSON bytes.
    pub fn from_json(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }

    /// Serialize this announcement to a JSON string.
    pub fn to_json_string(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize an announcement from a JSON string.
    pub fn from_json_string(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// Callback type for handling incoming VM placement announcements.
///
/// Implementations should update FDB and ARP proxy tables based on the
/// announcement contents.
pub type OnPlacementCallback = Box<dyn Fn(&VmPlacementAnnouncement) + Send + Sync>;

/// Broadcast a VM placement announcement to all fabric peers.
///
/// This function serializes the announcement and invokes the provided broadcast
/// hook. The actual transport mechanism (TCP, gRPC, etc.) is abstracted behind
/// the `broadcast_fn` callback, allowing the fabric layer to plug in its
/// existing peer communication channel.
///
/// Returns the serialized JSON bytes on success.
pub fn announce_vm_placement<F>(
    announcement: &VmPlacementAnnouncement,
    broadcast_fn: F,
) -> Result<Vec<u8>, serde_json::Error>
where
    F: FnOnce(&[u8]),
{
    let payload = announcement.to_json()?;

    info!(
        vpc_id = %announcement.vpc_id,
        vm_id = %announcement.vm_id,
        action = %announcement.action,
        hypervisor_id = %announcement.hypervisor_id,
        "broadcasting VM placement announcement"
    );

    debug!(
        vm_mac = %announcement.vm_mac,
        vm_ip = %announcement.vm_ip,
        subnet_id = %announcement.subnet_id,
        payload_size = payload.len(),
        "VM placement announcement payload"
    );

    broadcast_fn(&payload);

    Ok(payload)
}

/// Process an incoming VM placement announcement.
///
/// Deserializes the payload and invokes the provided callback. Returns the
/// deserialized announcement on success.
pub fn handle_vm_placement(
    payload: &[u8],
    callback: &OnPlacementCallback,
) -> Result<VmPlacementAnnouncement, serde_json::Error> {
    let announcement = VmPlacementAnnouncement::from_json(payload)?;

    info!(
        vpc_id = %announcement.vpc_id,
        vm_id = %announcement.vm_id,
        action = %announcement.action,
        hypervisor_id = %announcement.hypervisor_id,
        "received VM placement announcement"
    );

    callback(&announcement);

    Ok(announcement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn sample_add() -> VmPlacementAnnouncement {
        VmPlacementAnnouncement {
            vpc_id: "vpc-100".to_string(),
            vm_id: "vm-abc123".to_string(),
            vm_mac: "02:00:0a:00:01:05".to_string(),
            vm_ip: "10.0.1.5".to_string(),
            subnet_id: "subnet-frontend".to_string(),
            hypervisor_id: "fd12:3456:7800::1".to_string(),
            action: PlacementAction::Add,
        }
    }

    fn sample_remove() -> VmPlacementAnnouncement {
        VmPlacementAnnouncement {
            vpc_id: "vpc-100".to_string(),
            vm_id: "vm-abc123".to_string(),
            vm_mac: "02:00:0a:00:01:05".to_string(),
            vm_ip: "10.0.1.5".to_string(),
            subnet_id: "subnet-frontend".to_string(),
            hypervisor_id: "fd12:3456:7800::1".to_string(),
            action: PlacementAction::Remove,
        }
    }

    #[test]
    fn serialize_placement() {
        let placement = sample_add();
        let json = placement.to_json_string().unwrap();
        let deserialized = VmPlacementAnnouncement::from_json_string(&json).unwrap();
        assert_eq!(placement, deserialized);
    }

    #[test]
    fn deserialize_placement() {
        let json_str = r#"{
            "vpc_id": "vpc-200",
            "vm_id": "vm-def456",
            "vm_mac": "02:00:0a:00:02:03",
            "vm_ip": "10.0.2.3",
            "subnet_id": "subnet-database",
            "hypervisor_id": "fd12:3456:7800::2",
            "action": "add"
        }"#;
        let announcement = VmPlacementAnnouncement::from_json_string(json_str).unwrap();
        assert_eq!(announcement.vpc_id, "vpc-200");
        assert_eq!(announcement.vm_id, "vm-def456");
        assert_eq!(announcement.vm_mac, "02:00:0a:00:02:03");
        assert_eq!(announcement.vm_ip, "10.0.2.3");
        assert_eq!(announcement.subnet_id, "subnet-database");
        assert_eq!(announcement.hypervisor_id, "fd12:3456:7800::2");
        assert_eq!(announcement.action, PlacementAction::Add);
    }

    #[test]
    fn announce_add() {
        let placement = sample_add();
        let broadcast_payloads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let payloads = broadcast_payloads.clone();

        let result = announce_vm_placement(&placement, move |data| {
            payloads.lock().unwrap().push(data.to_vec());
        });

        assert!(result.is_ok());
        let sent = broadcast_payloads.lock().unwrap();
        assert_eq!(sent.len(), 1);

        let decoded = VmPlacementAnnouncement::from_json(&sent[0]).unwrap();
        assert_eq!(decoded.action, PlacementAction::Add);
        assert_eq!(decoded.vm_id, "vm-abc123");
        assert_eq!(decoded.vpc_id, "vpc-100");
    }

    #[test]
    fn announce_remove() {
        let placement = sample_remove();
        let broadcast_payloads: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let payloads = broadcast_payloads.clone();

        let result = announce_vm_placement(&placement, move |data| {
            payloads.lock().unwrap().push(data.to_vec());
        });

        assert!(result.is_ok());
        let sent = broadcast_payloads.lock().unwrap();
        assert_eq!(sent.len(), 1);

        let decoded = VmPlacementAnnouncement::from_json(&sent[0]).unwrap();
        assert_eq!(decoded.action, PlacementAction::Remove);
        assert_eq!(decoded.vm_id, "vm-abc123");
        assert_eq!(decoded.vpc_id, "vpc-100");
    }

    #[test]
    fn handle_incoming_placement() {
        let placement = sample_add();
        let payload = placement.to_json().unwrap();

        let received: Arc<Mutex<Vec<VmPlacementAnnouncement>>> = Arc::new(Mutex::new(Vec::new()));
        let rx = received.clone();

        let callback: OnPlacementCallback = Box::new(move |ann| {
            rx.lock().unwrap().push(ann.clone());
        });

        let result = handle_vm_placement(&payload, &callback);
        assert!(result.is_ok());

        let handled = received.lock().unwrap();
        assert_eq!(handled.len(), 1);
        assert_eq!(handled[0].action, PlacementAction::Add);
        assert_eq!(handled[0].vm_id, "vm-abc123");
    }

    #[test]
    fn json_bytes_roundtrip() {
        let placement = sample_add();
        let bytes = placement.to_json().unwrap();
        let decoded = VmPlacementAnnouncement::from_json(&bytes).unwrap();
        assert_eq!(placement, decoded);
    }

    #[test]
    fn action_display() {
        assert_eq!(PlacementAction::Add.to_string(), "add");
        assert_eq!(PlacementAction::Remove.to_string(), "remove");
    }

    #[test]
    fn action_serde_roundtrip() {
        let add_json = serde_json::to_string(&PlacementAction::Add).unwrap();
        assert_eq!(add_json, r#""add""#);
        let remove_json = serde_json::to_string(&PlacementAction::Remove).unwrap();
        assert_eq!(remove_json, r#""remove""#);

        let add: PlacementAction = serde_json::from_str(&add_json).unwrap();
        assert_eq!(add, PlacementAction::Add);
        let remove: PlacementAction = serde_json::from_str(&remove_json).unwrap();
        assert_eq!(remove, PlacementAction::Remove);
    }

    #[test]
    fn invalid_json_returns_error() {
        let result = VmPlacementAnnouncement::from_json_string("not valid json");
        assert!(result.is_err());
    }
}
