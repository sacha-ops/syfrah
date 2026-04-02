//! Remote VM creation — call a target Forge's HTTP API to create a VM.
//!
//! Used by the scheduler when the selected hypervisor is not the local node.
//! The leader calls `POST /v1/forge/instances` on the target Forge.

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Request to create a VM on a remote Forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCreateVmRequest {
    pub name: String,
    pub image: String,
    pub vcpus: u32,
    pub memory_mb: u32,
    pub subnet: Option<String>,
    pub project: Option<String>,
    pub org: Option<String>,
    pub ssh_key: Option<String>,
    pub disk_size_mb: Option<u32>,
    pub security_groups: Vec<String>,
    /// Placement zone constraint. Forwarded to the leader's Forge API
    /// so the scheduler can place the VM in the correct zone.
    #[serde(default)]
    pub zone: Option<String>,
    /// Pre-allocated IP from Raft IPAM (set by leader before dispatching).
    /// When set, the target Forge uses this IP instead of allocating locally,
    /// preventing duplicate IP assignments across nodes.
    #[serde(default)]
    pub pre_allocated_ip: Option<String>,
    /// Pre-allocated MAC from Raft IPAM (set by leader before dispatching).
    #[serde(default)]
    pub pre_allocated_mac: Option<String>,
}

/// Response from a remote VM creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteCreateVmResponse {
    pub success: bool,
    pub vm_id: Option<String>,
    pub ip: Option<String>,
    pub error: Option<String>,
}

/// Create a VM on a remote Forge via its HTTP API.
///
/// The target Forge listens on `[fabric_ipv6]:7100`.
pub async fn create_vm_on_remote(
    target_addr: &str,
    request: &RemoteCreateVmRequest,
) -> Result<RemoteCreateVmResponse, String> {
    // The ?direct=true parameter tells the target Forge to skip leader
    // forwarding and create the VM locally. Without it, the target would
    // forward the request back to the leader (since it's not the leader).
    let url = format!("http://{target_addr}/v1/instances?direct=true");
    info!(
        "remote_create: sending VM '{}' to Forge at {}",
        request.name, target_addr
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .post(&url)
        .json(request)
        .send()
        .await
        .map_err(|e| format!("failed to reach Forge at {target_addr}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            "remote_create: Forge at {} returned {}: {}",
            target_addr, status, body
        );
        return Ok(RemoteCreateVmResponse {
            success: false,
            vm_id: None,
            ip: None,
            error: Some(format!("Forge returned {status}: {body}")),
        });
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse Forge response: {e}"))?;

    // Extract VM info from the Forge response.
    let vm_id = body
        .get("id")
        .or_else(|| body.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ip = body
        .get("ip")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    info!(
        "remote_create: VM '{}' created on {} (ip={:?})",
        request.name, target_addr, ip
    );

    Ok(RemoteCreateVmResponse {
        success: true,
        vm_id,
        ip,
        error: None,
    })
}

/// Forward a VM creation request to the Raft leader's Forge API.
///
/// Unlike `create_vm_on_remote` (which uses `?direct=true` for placement on
/// a specific hypervisor), this does NOT use `?direct=true` so the leader
/// can run the scheduler with the zone constraint.
pub async fn forward_create_to_leader(
    leader_addr: &str,
    request: &RemoteCreateVmRequest,
) -> Result<RemoteCreateVmResponse, String> {
    let url = format!("http://{leader_addr}/v1/instances");
    info!(
        "forward_to_leader: sending VM '{}' to leader at {}",
        request.name, leader_addr
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("failed to build HTTP client: {e}"))?;

    let resp = client
        .post(&url)
        .json(request)
        .send()
        .await
        .map_err(|e| format!("failed to reach leader at {leader_addr}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        warn!(
            "forward_to_leader: leader at {} returned {}: {}",
            leader_addr, status, body
        );
        return Ok(RemoteCreateVmResponse {
            success: false,
            vm_id: None,
            ip: None,
            error: Some(format!("Forge returned {status}: {body}")),
        });
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("failed to parse leader response: {e}"))?;

    let vm_id = body
        .get("id")
        .or_else(|| body.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let ip = body
        .get("ip")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    Ok(RemoteCreateVmResponse {
        success: true,
        vm_id,
        ip,
        error: None,
    })
}

/// Look up a hypervisor's Forge address from its name.
///
/// The Forge HTTP API runs on port 7100 on the hypervisor's fabric IPv6 address.
/// Returns the address in `[ipv6]:7100` format.
pub fn forge_addr_from_fabric_ipv6(fabric_ipv6: &str) -> String {
    // If already bracketed, just add port
    if fabric_ipv6.starts_with('[') {
        format!("{fabric_ipv6}:7100")
    } else {
        format!("[{fabric_ipv6}]:7100")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_addr_from_ipv6() {
        assert_eq!(forge_addr_from_fabric_ipv6("fd00::1"), "[fd00::1]:7100");
        assert_eq!(forge_addr_from_fabric_ipv6("[fd00::1]"), "[fd00::1]:7100");
    }

    #[test]
    fn remote_request_serde() {
        let req = RemoteCreateVmRequest {
            name: "test-vm".to_string(),
            image: "alpine-3.20".to_string(),
            vcpus: 2,
            memory_mb: 2048,
            subnet: Some("web".to_string()),
            project: Some("backend".to_string()),
            org: Some("acme".to_string()),
            ssh_key: None,
            disk_size_mb: None,
            security_groups: vec!["default".to_string()],
            zone: Some("fsn1".to_string()),
            pre_allocated_ip: None,
            pre_allocated_mac: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let _: RemoteCreateVmRequest = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn remote_response_serde() {
        let resp = RemoteCreateVmResponse {
            success: true,
            vm_id: Some("test-vm".to_string()),
            ip: Some("10.0.0.5".to_string()),
            error: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let deser: RemoteCreateVmResponse = serde_json::from_str(&json).unwrap();
        assert!(deser.success);
        assert_eq!(deser.vm_id, Some("test-vm".to_string()));
    }
}
