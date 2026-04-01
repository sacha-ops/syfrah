use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::str::FromStr;
use std::sync::OnceLock;

use thiserror::Error;
use wireguard_control::{
    Backend, Device, DeviceUpdate, InterfaceName, Key, KeyPair, PeerConfigBuilder,
};

use syfrah_core::mesh::{PeerRecord, Topology};

use crate::config::{KeepalivePolicy, TopologyTier};

pub const DEFAULT_INTERFACE_NAME: &str = "syfrah0";

/// Global interface name, set once at daemon startup via [`set_interface_name`].
static INTERFACE_NAME: OnceLock<String> = OnceLock::new();

/// Set the WireGuard interface name for this process.
/// Must be called before any WireGuard operations. Subsequent calls are no-ops.
pub fn set_interface_name(name: &str) {
    let _ = INTERFACE_NAME.set(name.to_string());
}

/// Return the configured interface name (or the default).
pub fn interface_name() -> &'static str {
    INTERFACE_NAME
        .get()
        .map(|s| s.as_str())
        .unwrap_or(DEFAULT_INTERFACE_NAME)
}

#[derive(Debug, Error)]
pub enum WgError {
    #[error("wireguard interface error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid interface name: {0}")]
    InvalidName(String),
    #[error("invalid key: {0}")]
    InvalidKey(String),
    #[error("failed to assign IPv6 address: {0}")]
    AddressAssign(String),
    #[error("interface {0} not found")]
    NotFound(String),
    #[error("peer limit exceeded: {0} peers (max {1})")]
    PeerLimitExceeded(usize, usize),
}

fn backend() -> Backend {
    Backend::default()
}

fn iface_name() -> Result<InterfaceName, WgError> {
    InterfaceName::from_str(interface_name()).map_err(|e| WgError::InvalidName(e.to_string()))
}

/// Determine the topology tier of `peer` relative to the local node.
///
/// Returns [`TopologyTier::CrossRegion`] when either side has no topology
/// information, since unknown peers are treated conservatively.
pub fn resolve_tier(local_topology: Option<&Topology>, peer: &PeerRecord) -> TopologyTier {
    match (local_topology, peer.topology.as_ref()) {
        (Some(local), Some(remote)) => {
            if local.zone == remote.zone && local.region == remote.region {
                TopologyTier::SameZone
            } else if local.region == remote.region {
                TopologyTier::SameRegion
            } else {
                TopologyTier::CrossRegion
            }
        }
        _ => TopologyTier::CrossRegion,
    }
}

/// Return the keepalive interval for `peer` using the per-tier policy.
pub fn keepalive_for_peer(
    policy: &KeepalivePolicy,
    local_topology: Option<&Topology>,
    peer: &PeerRecord,
) -> u16 {
    policy.for_tier(resolve_tier(local_topology, peer))
}

/// Generate a new WireGuard keypair.
pub fn generate_keypair() -> KeyPair {
    KeyPair::generate()
}

/// Create the WireGuard interface and set the private key + listen port.
/// If the interface already exists, it will be reconfigured.
pub fn create_interface(private_key: &Key, listen_port: u16) -> Result<(), WgError> {
    let iface = iface_name()?;
    DeviceUpdate::new()
        .set_private_key(private_key.clone())
        .set_listen_port(listen_port)
        .apply(&iface, backend())?;
    Ok(())
}

/// Destroy the WireGuard interface if it exists.
/// Returns `Ok(())` when the interface is already gone (ENODEV / os error 19).
pub fn destroy_interface() -> Result<(), WgError> {
    let iface = iface_name()?;
    let device = match Device::get(&iface, backend()) {
        Ok(d) => d,
        Err(e) if e.raw_os_error() == Some(19) => return Ok(()), // ENODEV — already gone
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(WgError::Io(e)),
    };
    match device.delete() {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(19) => Ok(()), // ENODEV — gone between get and delete
        Err(e) => Err(WgError::Io(e)),
    }
}

/// Read the current state of the WireGuard interface.
pub fn get_device() -> Result<Device, WgError> {
    let iface = iface_name()?;
    Device::get(&iface, backend()).map_err(WgError::Io)
}

/// Full reconciliation: replace all peers on the interface with the given peer records.
/// Skips peers whose WG public key matches `self_pubkey` (the local node).
///
/// WARNING: This uses `replace_peers()` which tears down all existing WireGuard
/// sessions and forces new handshakes. Use `sync_peers()` instead for non-disruptive
/// reconciliation. This function is kept only for initial setup (join/start) where
/// no active sessions exist yet.
pub fn apply_peers(
    self_pubkey: &Key,
    peers: &[PeerRecord],
    keepalive_interval: u16,
) -> Result<(), WgError> {
    apply_peers_tiered(self_pubkey, peers, keepalive_interval, None, None)
}

/// Full reconciliation with per-topology-tier keepalive support.
///
/// When `policy` and `local_topology` are provided, each peer gets a
/// keepalive interval determined by its topology tier. When they are
/// `None`, the flat `default_keepalive` is used for every peer.
pub fn apply_peers_tiered(
    self_pubkey: &Key,
    peers: &[PeerRecord],
    default_keepalive: u16,
    policy: Option<&KeepalivePolicy>,
    local_topology: Option<&Topology>,
) -> Result<(), WgError> {
    let iface = iface_name()?;

    let mut update = DeviceUpdate::new().replace_peers();

    for peer in peers {
        let peer_key = Key::from_base64(&peer.wg_public_key)
            .map_err(|_| WgError::InvalidKey(peer.wg_public_key.clone()))?;

        // Don't add ourselves as a peer
        if peer_key == *self_pubkey {
            continue;
        }

        // Skip removed peers
        if peer.status == syfrah_core::mesh::PeerStatus::Removed {
            continue;
        }

        let ka = policy
            .map(|p| keepalive_for_peer(p, local_topology, peer))
            .unwrap_or(default_keepalive);

        let peer_config = PeerConfigBuilder::new(&peer_key)
            .set_endpoint(peer.endpoint)
            .replace_allowed_ips()
            .add_allowed_ip(IpAddr::V6(peer.mesh_ipv6), 128)
            .set_persistent_keepalive_interval(ka);

        update = update.add_peer(peer_config);
    }

    update.apply(&iface, backend())?;

    // Add routes for each peer's mesh IPv6 via the WireGuard interface
    for peer in peers {
        if peer.status == syfrah_core::mesh::PeerStatus::Removed {
            continue;
        }
        let peer_key = Key::from_base64(&peer.wg_public_key)
            .map_err(|_| WgError::InvalidKey(peer.wg_public_key.clone()))?;
        if peer_key == *self_pubkey {
            continue;
        }
        add_route_v6(peer.mesh_ipv6)?;
    }

    Ok(())
}

/// Compute the diff between desired peer state and current WireGuard device peers.
///
/// Returns `(to_add_or_update, to_remove)`:
/// - `to_add_or_update`: peers in `desired` that are missing from WG or have changed allowed IPs.
/// - `to_remove`: public keys present in WG but not in the desired set.
///
/// Endpoint differences are intentionally ignored: WireGuard handles endpoint
/// roaming natively. Re-applying a peer just because the observed endpoint
/// differs from the stored one resets the session and causes packet loss.
///
/// This is a pure function suitable for unit testing.
pub fn diff_peers(
    self_pubkey: &Key,
    desired: &[PeerRecord],
    wg_peers: &[PeerSummary],
) -> Result<(Vec<PeerRecord>, Vec<String>), WgError> {
    use std::collections::{HashMap, HashSet};

    // Build a map of current WG peers: pubkey -> allowed_ips set
    let wg_map: HashMap<&str, HashSet<&str>> = wg_peers
        .iter()
        .map(|p| {
            let ips: HashSet<&str> = p.allowed_ips.iter().map(|s| s.as_str()).collect();
            (p.public_key.as_str(), ips)
        })
        .collect();

    let self_b64 = self_pubkey.to_base64();

    // Determine desired set (non-removed, non-self)
    let mut desired_keys = HashSet::new();
    let mut to_add_or_update = Vec::new();

    for peer in desired {
        if peer.wg_public_key == self_b64 {
            continue;
        }
        if peer.status == syfrah_core::mesh::PeerStatus::Removed {
            continue;
        }
        desired_keys.insert(peer.wg_public_key.as_str());

        let desired_aip = format!("{}/128", peer.mesh_ipv6);

        match wg_map.get(peer.wg_public_key.as_str()) {
            Some(existing_ips) if existing_ips.contains(desired_aip.as_str()) => {
                // Peer exists with correct allowed IPs — normally no change needed.
                // However, if the stored endpoint is 0.0.0.0 (unspecified), we must
                // re-apply the peer so WireGuard can learn a valid endpoint. The
                // general rule of ignoring endpoint differences for roaming does not
                // apply when the stored endpoint is fundamentally unreachable.
                if peer.endpoint.ip().is_unspecified() {
                    to_add_or_update.push(peer.clone());
                }
            }
            Some(_) => {
                // Peer exists but allowed IPs changed — update
                to_add_or_update.push(peer.clone());
            }
            None => {
                // Peer missing from WG — add
                to_add_or_update.push(peer.clone());
            }
        }
    }

    // Peers in WG but not in desired set should be removed (never remove self)
    let to_remove: Vec<String> = wg_peers
        .iter()
        .filter(|p| !desired_keys.contains(p.public_key.as_str()) && p.public_key != self_b64)
        .map(|p| p.public_key.clone())
        .collect();

    Ok((to_add_or_update, to_remove))
}

/// Non-disruptive peer sync: only add/remove/update peers that actually changed.
///
/// Unlike `apply_peers`, this does NOT use `replace_peers()` and therefore
/// preserves existing WireGuard sessions and in-flight traffic.
///
/// Returns the number of peers that were added, updated, or removed.
pub fn sync_peers(
    self_pubkey: &Key,
    desired: &[PeerRecord],
    keepalive_interval: u16,
) -> Result<usize, WgError> {
    sync_peers_tiered(self_pubkey, desired, keepalive_interval, None, None)
}

/// Non-disruptive peer sync with per-topology-tier keepalive support.
///
/// When `policy` and `local_topology` are provided, each peer gets a
/// keepalive interval determined by its topology tier. Otherwise the flat
/// `default_keepalive` is used.
pub fn sync_peers_tiered(
    self_pubkey: &Key,
    desired: &[PeerRecord],
    default_keepalive: u16,
    policy: Option<&KeepalivePolicy>,
    local_topology: Option<&Topology>,
) -> Result<usize, WgError> {
    let summary = interface_summary()?;
    let (to_add_or_update, to_remove) = diff_peers(self_pubkey, desired, &summary.peers)?;

    let changes = to_add_or_update.len() + to_remove.len();
    if changes == 0 {
        return Ok(0);
    }

    let iface = iface_name()?;

    // Build a lookup so we can clean up routes for removed peers
    let peer_allowed: std::collections::HashMap<&str, &[String]> = summary
        .peers
        .iter()
        .map(|p| (p.public_key.as_str(), p.allowed_ips.as_slice()))
        .collect();

    // Remove peers that are no longer desired, including route cleanup
    for pubkey_b64 in &to_remove {
        let key =
            Key::from_base64(pubkey_b64).map_err(|_| WgError::InvalidKey(pubkey_b64.clone()))?;
        DeviceUpdate::new()
            .remove_peer_by_key(&key)
            .apply(&iface, backend())?;

        // Clean up /128 routes for the removed peer
        if let Some(allowed_ips) = peer_allowed.get(pubkey_b64.as_str()) {
            for aip in *allowed_ips {
                // allowed_ips entries look like "fd00::1/128"
                if let Some(addr_str) = aip.strip_suffix("/128") {
                    if let Ok(v6) = addr_str.parse::<std::net::Ipv6Addr>() {
                        let _ = remove_route_v6(v6);
                    }
                }
            }
        }
    }

    // Add or update peers that are new or changed
    for peer in &to_add_or_update {
        let ka = policy
            .map(|p| keepalive_for_peer(p, local_topology, peer))
            .unwrap_or(default_keepalive);
        upsert_peer(self_pubkey, peer, ka)?;
    }

    Ok(changes)
}

/// Incrementally add or update a single peer, enforcing a maximum peer count.
/// Returns `Err(WgError::PeerLimitExceeded)` if the peer is new and the
/// current peer count already meets or exceeds `max_peers`.
///
/// `peer_count` and `peer_exists` are caller-supplied so that we avoid an
/// O(n) `get_device()` call on every invocation.
///
/// **Note:** Because the caller reads these values before calling this function,
/// there is a TOCTOU window: concurrent upserts may both pass the limit check.
/// The WG-level peer cap is therefore a soft limit, not a hard guarantee.
/// The *store's* `upsert_peer_bounded` remains the hard enforcement point: it
/// performs an atomic exists-check + count + insert within a single DB transaction,
/// so the store will never persist more than `max_peers` entries even under races.
pub fn upsert_peer_bounded(
    self_pubkey: &Key,
    peer: &PeerRecord,
    max_peers: usize,
    peer_count: usize,
    peer_exists: bool,
    keepalive_interval: u16,
) -> Result<(), WgError> {
    let peer_key = Key::from_base64(&peer.wg_public_key)
        .map_err(|_| WgError::InvalidKey(peer.wg_public_key.clone()))?;

    if peer_key == *self_pubkey {
        return Ok(());
    }

    // For non-removal operations, check the limit
    if peer.status != syfrah_core::mesh::PeerStatus::Removed
        && !peer_exists
        && peer_count >= max_peers
    {
        return Err(WgError::PeerLimitExceeded(peer_count, max_peers));
    }

    upsert_peer(self_pubkey, peer, keepalive_interval)
}

/// Incrementally add or update a single peer. Does NOT replace all peers.
/// Use this for gossip events (one peer changed at a time).
pub fn upsert_peer(
    self_pubkey: &Key,
    peer: &PeerRecord,
    keepalive_interval: u16,
) -> Result<(), WgError> {
    let peer_key = Key::from_base64(&peer.wg_public_key)
        .map_err(|_| WgError::InvalidKey(peer.wg_public_key.clone()))?;

    if peer_key == *self_pubkey {
        return Ok(());
    }

    let iface = iface_name()?;

    if peer.status == syfrah_core::mesh::PeerStatus::Removed {
        // Remove this peer
        DeviceUpdate::new()
            .remove_peer_by_key(&peer_key)
            .apply(&iface, backend())?;
        remove_route_v6(peer.mesh_ipv6)?;
    } else {
        // Add or update this peer (layered on top, no replace_peers)
        let peer_config = PeerConfigBuilder::new(&peer_key)
            .set_endpoint(peer.endpoint)
            .replace_allowed_ips()
            .add_allowed_ip(IpAddr::V6(peer.mesh_ipv6), 128)
            .set_persistent_keepalive_interval(keepalive_interval);

        DeviceUpdate::new()
            .add_peer(peer_config)
            .apply(&iface, backend())?;
        add_route_v6(peer.mesh_ipv6)?;
    }

    Ok(())
}

/// Remove an IPv6 route for a peer's mesh address.
fn remove_route_v6(addr: std::net::Ipv6Addr) -> Result<(), WgError> {
    let cidr = format!("{addr}/128");

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("ip")
            .args(["-6", "route", "del", &cidr, "dev", interface_name()])
            .output();
    }

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("route")
            .args(["-n", "delete", "-inet6", &cidr])
            .output();
    }

    Ok(())
}

/// Assign an IPv6 address to the WireGuard interface.
/// This calls system commands (ip on Linux, ifconfig on macOS).
pub fn assign_ipv6(addr: Ipv6Addr) -> Result<(), WgError> {
    let cidr = format!("{addr}/128");

    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ip")
            .args(["-6", "addr", "add", &cidr, "dev", interface_name()])
            .output()
            .map_err(|e| WgError::AddressAssign(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Address already assigned is OK (idempotent setup).
            // Older iproute2: "RTNETLINK answers: File exists"
            // Newer iproute2: "Error: ipv6: address already assigned."
            if !stderr.contains("File exists") && !stderr.contains("already assigned") {
                return Err(WgError::AddressAssign(stderr.into_owned()));
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ifconfig")
            .args([interface_name(), "inet6", &cidr])
            .output()
            .map_err(|e| WgError::AddressAssign(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WgError::AddressAssign(stderr.into_owned()));
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        return Err(WgError::AddressAssign(
            "unsupported platform for IPv6 address assignment".into(),
        ));
    }

    Ok(())
}

/// Add an IPv6 route for a peer's mesh address via the WireGuard interface.
fn add_route_v6(addr: std::net::Ipv6Addr) -> Result<(), WgError> {
    let cidr = format!("{addr}/128");

    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ip")
            .args(["-6", "route", "replace", &cidr, "dev", interface_name()])
            .output()
            .map_err(|e| WgError::AddressAssign(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("failed to add route for {cidr}: {stderr}");
        }
    }

    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("route")
            .args(["-n", "add", "-inet6", &cidr, "-interface", interface_name()])
            .output()
            .map_err(|e| WgError::AddressAssign(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "File exists" (route already present)
            if !stderr.contains("File exists") {
                tracing::warn!("failed to add route for {cidr}: {stderr}");
            }
        }
    }

    Ok(())
}

/// Bring the interface up (Linux only; macOS interfaces are up after ifconfig).
pub fn bring_interface_up() -> Result<(), WgError> {
    #[cfg(target_os = "linux")]
    {
        let output = std::process::Command::new("ip")
            .args(["link", "set", interface_name(), "up"])
            .output()
            .map_err(|e| WgError::AddressAssign(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(WgError::AddressAssign(stderr.into_owned()));
        }
    }

    Ok(())
}

/// Convenience: full setup sequence for a node.
/// Creates the interface, sets the key, assigns the IPv6 address, brings it up.
pub fn setup_interface(
    keypair: &KeyPair,
    listen_port: u16,
    mesh_ipv6: Ipv6Addr,
) -> Result<(), WgError> {
    create_interface(&keypair.private, listen_port)?;
    if let Err(e) = assign_ipv6(mesh_ipv6) {
        // Rollback: destroy the interface we just created
        let _ = destroy_interface();
        return Err(e);
    }
    if let Err(e) = bring_interface_up() {
        let _ = destroy_interface();
        return Err(e);
    }
    Ok(())
}

/// Convenience: full teardown.
pub fn teardown_interface() -> Result<(), WgError> {
    destroy_interface()
}

/// Get a summary of the current interface state for display.
pub fn interface_summary() -> Result<InterfaceSummary, WgError> {
    let device = get_device()?;
    Ok(InterfaceSummary {
        name: interface_name().to_string(),
        public_key: device.public_key.map(|k| k.to_base64()),
        listen_port: device.listen_port,
        peer_count: device.peers.len(),
        peers: device
            .peers
            .iter()
            .map(|p| PeerSummary {
                public_key: p.config.public_key.to_base64(),
                endpoint: p.config.endpoint,
                allowed_ips: p
                    .config
                    .allowed_ips
                    .iter()
                    .map(|ip| format!("{}/{}", ip.address, ip.cidr))
                    .collect(),
                last_handshake: p.stats.last_handshake_time,
                rx_bytes: p.stats.rx_bytes,
                tx_bytes: p.stats.tx_bytes,
            })
            .collect(),
    })
}

#[derive(Debug, Clone)]
pub struct InterfaceSummary {
    pub name: String,
    pub public_key: Option<String>,
    pub listen_port: Option<u16>,
    pub peer_count: usize,
    pub peers: Vec<PeerSummary>,
}

#[derive(Debug, Clone)]
pub struct PeerSummary {
    pub public_key: String,
    pub endpoint: Option<SocketAddr>,
    pub allowed_ips: Vec<String>,
    pub last_handshake: Option<std::time::SystemTime>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keypair_generation() {
        let kp = generate_keypair();
        assert_ne!(kp.private, kp.public);
        // Public key derivation is deterministic
        assert_eq!(kp.private.get_public(), kp.public);
    }

    #[test]
    fn keypair_base64_roundtrip() {
        let kp = generate_keypair();
        let b64 = kp.public.to_base64();
        let parsed = Key::from_base64(&b64).unwrap();
        assert_eq!(parsed, kp.public);
    }

    #[test]
    fn iface_name_valid() {
        let name = iface_name().unwrap();
        assert_eq!(name.as_str_lossy(), interface_name());
    }

    fn make_peer(pubkey: &str, status: syfrah_core::mesh::PeerStatus) -> PeerRecord {
        PeerRecord {
            name: "test-peer".into(),
            wg_public_key: pubkey.into(),
            endpoint: "203.0.113.1:51820".parse().unwrap(),
            mesh_ipv6: "fd12:3456:7800::1".parse().unwrap(),
            last_seen: 0,
            status,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        }
    }

    #[test]
    fn upsert_peer_bounded_rejects_new_peer_at_limit() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        let result = upsert_peer_bounded(&self_kp.public, &peer, 10, 10, false, 25);
        assert!(matches!(result, Err(WgError::PeerLimitExceeded(10, 10))));
    }

    #[test]
    fn upsert_peer_bounded_rejects_new_peer_over_limit() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        let result = upsert_peer_bounded(&self_kp.public, &peer, 5, 7, false, 25);
        assert!(matches!(result, Err(WgError::PeerLimitExceeded(7, 5))));
    }

    #[test]
    fn upsert_peer_bounded_skips_self() {
        let self_kp = generate_keypair();
        let peer = make_peer(
            &self_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        // Should succeed (no-op) even at limit, because it's self
        let result = upsert_peer_bounded(&self_kp.public, &peer, 0, 0, false, 25);
        assert!(result.is_ok());
    }

    #[test]
    fn upsert_peer_bounded_rejects_at_zero_max() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        let result = upsert_peer_bounded(&self_kp.public, &peer, 0, 0, false, 25);
        assert!(matches!(result, Err(WgError::PeerLimitExceeded(0, 0))));
    }

    #[test]
    fn upsert_peer_bounded_allows_existing_peer_at_limit() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        // Existing peer (peer_exists=true) should pass even when at limit
        let result = upsert_peer_bounded(&self_kp.public, &peer, 10, 10, true, 25);
        // This calls upsert_peer which requires a real WG interface, so it will
        // fail with a WG error — but it should NOT fail with PeerLimitExceeded.
        assert!(!matches!(result, Err(WgError::PeerLimitExceeded(_, _))));
    }

    #[test]
    fn upsert_peer_bounded_allows_removal_at_limit() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Removed,
        );

        // Removed peer should pass even when at limit and not previously known
        let result = upsert_peer_bounded(&self_kp.public, &peer, 10, 10, false, 25);
        assert!(!matches!(result, Err(WgError::PeerLimitExceeded(_, _))));
    }

    #[test]
    fn upsert_peer_bounded_allows_new_peer_under_limit() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer = make_peer(
            &peer_kp.public.to_base64(),
            syfrah_core::mesh::PeerStatus::Active,
        );

        // New peer under limit should pass the limit check
        let result = upsert_peer_bounded(&self_kp.public, &peer, 10, 5, false, 25);
        assert!(!matches!(result, Err(WgError::PeerLimitExceeded(_, _))));
    }

    #[test]
    fn upsert_peer_bounded_invalid_key() {
        let self_kp = generate_keypair();
        let peer = make_peer("not-valid-base64", syfrah_core::mesh::PeerStatus::Active);

        let result = upsert_peer_bounded(&self_kp.public, &peer, 10, 0, false, 25);
        assert!(matches!(result, Err(WgError::InvalidKey(_))));
    }

    // ── diff_peers tests ──

    fn make_wg_summary(pubkey: &str, endpoint: &str) -> PeerSummary {
        PeerSummary {
            public_key: pubkey.to_string(),
            endpoint: Some(endpoint.parse().unwrap()),
            // Default allowed IP matches make_peer's mesh_ipv6
            allowed_ips: vec!["fd12:3456:7800::1/128".to_string()],
            last_handshake: None,
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }

    #[test]
    fn diff_peers_no_change() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        let desired = vec![make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active)];
        let wg_peers = vec![make_wg_summary(&peer_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(add.is_empty(), "expected no peers to add/update");
        assert!(remove.is_empty(), "expected no peers to remove");
    }

    #[test]
    fn diff_peers_missing_peer() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        let desired = vec![make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active)];
        let wg_peers: Vec<PeerSummary> = vec![];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].wg_public_key, peer_b64);
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_stale_peer_removed() {
        let self_kp = generate_keypair();
        let stale_kp = generate_keypair();
        let stale_b64 = stale_kp.public.to_base64();

        let desired: Vec<PeerRecord> = vec![];
        let wg_peers = vec![make_wg_summary(&stale_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(add.is_empty());
        assert_eq!(remove.len(), 1);
        assert_eq!(remove[0], stale_b64);
    }

    #[test]
    fn diff_peers_endpoint_changed_no_update() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        // Desired endpoint differs from WG endpoint — should NOT trigger update
        // because WireGuard handles endpoint roaming natively.
        let mut desired_peer = make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active);
        desired_peer.endpoint = "198.51.100.5:51820".parse().unwrap();
        let desired = vec![desired_peer];
        let wg_peers = vec![make_wg_summary(&peer_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(
            add.is_empty(),
            "endpoint-only change must not trigger update (WG handles roaming)"
        );
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_allowed_ip_changed() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        // Desired mesh_ipv6 differs from WG allowed IPs — should trigger update
        let mut desired_peer = make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active);
        desired_peer.mesh_ipv6 = "fd12:3456:7800::99".parse().unwrap();
        let desired = vec![desired_peer];
        let wg_peers = vec![make_wg_summary(&peer_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert_eq!(add.len(), 1, "allowed IP change should trigger update");
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_skips_self() {
        let self_kp = generate_keypair();
        let self_b64 = self_kp.public.to_base64();

        let desired = vec![make_peer(&self_b64, syfrah_core::mesh::PeerStatus::Active)];
        let wg_peers: Vec<PeerSummary> = vec![];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(add.is_empty(), "self should be skipped");
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_skips_removed() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        let desired = vec![make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Removed)];
        let wg_peers: Vec<PeerSummary> = vec![];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(add.is_empty(), "removed peers should be skipped");
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_mixed_scenario() {
        let self_kp = generate_keypair();
        let peer_a = generate_keypair();
        let peer_b = generate_keypair();
        let peer_c = generate_keypair();
        let a_b64 = peer_a.public.to_base64();
        let b_b64 = peer_b.public.to_base64();
        let c_b64 = peer_c.public.to_base64();

        // peer_a: exists in WG with same endpoint (no change)
        // peer_b: missing from WG (needs add)
        // peer_c: in WG but not in desired (needs remove)
        let desired = vec![
            make_peer(&a_b64, syfrah_core::mesh::PeerStatus::Active),
            make_peer(&b_b64, syfrah_core::mesh::PeerStatus::Active),
        ];
        let wg_peers = vec![
            make_wg_summary(&a_b64, "203.0.113.1:51820"),
            make_wg_summary(&c_b64, "203.0.113.1:51820"),
        ];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].wg_public_key, b_b64);
        assert_eq!(remove.len(), 1);
        assert_eq!(remove[0], c_b64);
    }

    #[test]
    fn diff_peers_does_not_remove_self_key() {
        let self_kp = generate_keypair();
        let self_b64 = self_kp.public.to_base64();

        // Self key appears in WG peers but not in desired — should NOT be removed
        let desired: Vec<PeerRecord> = vec![];
        let wg_peers = vec![make_wg_summary(&self_b64, "203.0.113.1:51820")];

        let (_add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(remove.is_empty(), "self key must never appear in to_remove");
    }

    #[test]
    fn diff_peers_removes_removed_peer_still_in_wg() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        // Peer is Removed in desired set AND still present in WireGuard — should be removed
        let desired = vec![make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Removed)];
        let wg_peers = vec![make_wg_summary(&peer_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(add.is_empty(), "removed peer should not be added");
        assert_eq!(remove.len(), 1);
        assert_eq!(
            remove[0], peer_b64,
            "removed peer still in WG should be in to_remove"
        );
    }

    // ── diff_peers: 0.0.0.0 endpoint triggers update (issue #285) ──

    #[test]
    fn diff_peers_zero_endpoint_triggers_update() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        // Peer exists in WG with correct allowed IPs, but stored endpoint
        // is 0.0.0.0 — should trigger an update so WG can learn a real endpoint.
        let mut desired_peer = make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active);
        desired_peer.endpoint = "0.0.0.0:51820".parse().unwrap();
        let desired = vec![desired_peer];
        let wg_peers = vec![make_wg_summary(&peer_b64, "203.0.113.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert_eq!(
            add.len(),
            1,
            "peer with 0.0.0.0 endpoint must be re-applied even if allowed IPs match"
        );
        assert_eq!(add[0].wg_public_key, peer_b64);
        assert!(remove.is_empty());
    }

    #[test]
    fn diff_peers_valid_endpoint_no_spurious_update() {
        let self_kp = generate_keypair();
        let peer_kp = generate_keypair();
        let peer_b64 = peer_kp.public.to_base64();

        // Peer with a valid (non-zero) endpoint and matching allowed IPs —
        // should NOT trigger an update (normal roaming case).
        let desired = vec![make_peer(&peer_b64, syfrah_core::mesh::PeerStatus::Active)];
        let wg_peers = vec![make_wg_summary(&peer_b64, "198.51.100.1:51820")];

        let (add, remove) = diff_peers(&self_kp.public, &desired, &wg_peers).unwrap();
        assert!(
            add.is_empty(),
            "valid endpoint with matching IPs should not trigger update"
        );
        assert!(remove.is_empty());
    }

    // Integration tests (require root) are skipped in normal CI.
    // Run with: sudo cargo test -- --ignored
    #[test]
    #[ignore]
    fn create_and_destroy_interface() {
        let kp = generate_keypair();
        create_interface(&kp.private, 51820).unwrap();

        let device = get_device().unwrap();
        assert_eq!(device.public_key, Some(kp.public.clone()));
        assert_eq!(device.listen_port, Some(51820));

        destroy_interface().unwrap();
        assert!(get_device().is_err());
    }

    #[test]
    #[ignore]
    fn apply_peers_integration() {
        let kp = generate_keypair();
        create_interface(&kp.private, 51820).unwrap();

        let peer_kp = generate_keypair();
        let peer = PeerRecord {
            name: "test-peer".into(),
            wg_public_key: peer_kp.public.to_base64(),
            endpoint: "203.0.113.1:51820".parse().unwrap(),
            mesh_ipv6: "fd12:3456:7800::1".parse().unwrap(),
            last_seen: 0,
            status: syfrah_core::mesh::PeerStatus::Active,
            region: None,
            zone: None,
            topology: None,
            hypervisor_report: None,
        };

        apply_peers(&kp.public, &[peer], 25).unwrap();

        let device = get_device().unwrap();
        assert_eq!(device.peers.len(), 1);
        assert_eq!(device.peers[0].config.public_key, peer_kp.public);

        destroy_interface().unwrap();
    }

    // ── resolve_tier / keepalive_for_peer tests ──

    fn make_topology(region: &str, zone: &str) -> Topology {
        use syfrah_core::mesh::{Region, Zone};
        Topology {
            region: Region::new(region).unwrap(),
            zone: Zone::new(zone).unwrap(),
        }
    }

    fn make_peer_with_topo(
        pubkey: &str,
        status: syfrah_core::mesh::PeerStatus,
        topo: Option<Topology>,
    ) -> PeerRecord {
        PeerRecord {
            name: "test-peer".into(),
            wg_public_key: pubkey.into(),
            endpoint: "203.0.113.1:51820".parse().unwrap(),
            mesh_ipv6: "fd12:3456:7800::1".parse().unwrap(),
            last_seen: 0,
            status,
            region: topo.as_ref().map(|t| t.region.as_str().to_owned()),
            zone: topo.as_ref().map(|t| t.zone.as_str().to_owned()),
            topology: topo,
            hypervisor_report: None,
        }
    }

    #[test]
    fn resolve_tier_same_zone() {
        let local = make_topology("eu-west", "eu-west-1a");
        let peer_topo = make_topology("eu-west", "eu-west-1a");
        let peer = make_peer_with_topo(
            "dGVzdA==",
            syfrah_core::mesh::PeerStatus::Active,
            Some(peer_topo),
        );
        assert_eq!(resolve_tier(Some(&local), &peer), TopologyTier::SameZone);
    }

    #[test]
    fn resolve_tier_same_region() {
        let local = make_topology("eu-west", "eu-west-1a");
        let peer_topo = make_topology("eu-west", "eu-west-1b");
        let peer = make_peer_with_topo(
            "dGVzdA==",
            syfrah_core::mesh::PeerStatus::Active,
            Some(peer_topo),
        );
        assert_eq!(resolve_tier(Some(&local), &peer), TopologyTier::SameRegion);
    }

    #[test]
    fn resolve_tier_cross_region() {
        let local = make_topology("eu-west", "eu-west-1a");
        let peer_topo = make_topology("us-east", "us-east-1a");
        let peer = make_peer_with_topo(
            "dGVzdA==",
            syfrah_core::mesh::PeerStatus::Active,
            Some(peer_topo),
        );
        assert_eq!(resolve_tier(Some(&local), &peer), TopologyTier::CrossRegion);
    }

    #[test]
    fn resolve_tier_unknown_topology_is_cross_region() {
        let local = make_topology("eu-west", "eu-west-1a");
        let peer = make_peer_with_topo("dGVzdA==", syfrah_core::mesh::PeerStatus::Active, None);
        assert_eq!(resolve_tier(Some(&local), &peer), TopologyTier::CrossRegion);
    }

    #[test]
    fn resolve_tier_no_local_topology_is_cross_region() {
        let peer_topo = make_topology("eu-west", "eu-west-1a");
        let peer = make_peer_with_topo(
            "dGVzdA==",
            syfrah_core::mesh::PeerStatus::Active,
            Some(peer_topo),
        );
        assert_eq!(resolve_tier(None, &peer), TopologyTier::CrossRegion);
    }

    #[test]
    fn keepalive_for_peer_uses_policy() {
        let policy = KeepalivePolicy {
            same_zone_keepalive: 10,
            same_region_keepalive: 20,
            cross_region_keepalive: 40,
        };
        let local = make_topology("eu-west", "eu-west-1a");

        let sz = make_peer_with_topo(
            "a",
            syfrah_core::mesh::PeerStatus::Active,
            Some(make_topology("eu-west", "eu-west-1a")),
        );
        assert_eq!(keepalive_for_peer(&policy, Some(&local), &sz), 10);

        let sr = make_peer_with_topo(
            "b",
            syfrah_core::mesh::PeerStatus::Active,
            Some(make_topology("eu-west", "eu-west-1b")),
        );
        assert_eq!(keepalive_for_peer(&policy, Some(&local), &sr), 20);

        let cr = make_peer_with_topo(
            "c",
            syfrah_core::mesh::PeerStatus::Active,
            Some(make_topology("us-east", "us-east-1a")),
        );
        assert_eq!(keepalive_for_peer(&policy, Some(&local), &cr), 40);
    }
}
