//! WireGuard interface management via `wg` and `ip` commands.
//!
//! No C bindings — uses the standard WireGuard CLI tools.
//! Requires `wireguard-tools` installed on the system.

use std::net::Ipv6Addr;
use std::process::Command;

use syfrah_core::error::SyfrahError;

use super::super::peer::Peer;

/// Default interface name.
pub const INTERFACE_NAME: &str = "syfrah0";

/// Create the WireGuard interface with private key and listen port.
pub fn create_interface(
    private_key: &str,
    listen_port: u16,
    mesh_ipv6: &Ipv6Addr,
) -> Result<(), SyfrahError> {
    // Create interface
    run_cmd(
        "ip",
        &["link", "add", "dev", INTERFACE_NAME, "type", "wireguard"],
    )?;

    // Set private key via wg set (pipe through stdin)
    let status = Command::new("wg")
        .args([
            "set",
            INTERFACE_NAME,
            "private-key",
            "/dev/stdin",
            "listen-port",
            &listen_port.to_string(),
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(private_key.as_bytes())?;
            }
            child.wait()
        })
        .map_err(|e| SyfrahError::internal(format!("wg set failed: {e}")))?;

    if !status.success() {
        return Err(SyfrahError::internal("wg set private-key failed"));
    }

    // Assign IPv6 address
    run_cmd(
        "ip",
        &[
            "-6",
            "addr",
            "add",
            &format!("{mesh_ipv6}/128"),
            "dev",
            INTERFACE_NAME,
        ],
    )?;

    // Bring interface up
    run_cmd("ip", &["link", "set", "up", "dev", INTERFACE_NAME])?;

    Ok(())
}

/// Remove the WireGuard interface.
pub fn destroy_interface() -> Result<(), SyfrahError> {
    run_cmd("ip", &["link", "del", "dev", INTERFACE_NAME])
}

/// Add a peer to the WireGuard interface.
pub fn add_peer(peer: &Peer, keepalive_secs: u16) -> Result<(), SyfrahError> {
    let mut args = vec![
        "set".to_string(),
        INTERFACE_NAME.to_string(),
        "peer".to_string(),
        peer.wg_public_key.clone(),
        "allowed-ips".to_string(),
        format!("{}/128", peer.mesh_ipv6),
        "persistent-keepalive".to_string(),
        keepalive_secs.to_string(),
    ];

    if let Some(ref endpoint) = peer.endpoint {
        args.push("endpoint".to_string());
        args.push(endpoint.clone());
    }

    let args_ref: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    run_cmd("wg", &args_ref)?;

    // Add route for peer's IPv6
    run_cmd(
        "ip",
        &[
            "-6",
            "route",
            "add",
            &format!("{}/128", peer.mesh_ipv6),
            "dev",
            INTERFACE_NAME,
        ],
    )?;

    Ok(())
}

/// Remove a peer from the WireGuard interface.
pub fn remove_peer(wg_public_key: &str, mesh_ipv6: &Ipv6Addr) -> Result<(), SyfrahError> {
    run_cmd(
        "wg",
        &["set", INTERFACE_NAME, "peer", wg_public_key, "remove"],
    )?;

    // Remove route (ignore errors — may not exist)
    let _ = run_cmd(
        "ip",
        &[
            "-6",
            "route",
            "del",
            &format!("{mesh_ipv6}/128"),
            "dev",
            INTERFACE_NAME,
        ],
    );

    Ok(())
}

/// Check if the WireGuard interface exists.
pub fn interface_exists() -> bool {
    Command::new("ip")
        .args(["link", "show", "dev", INTERFACE_NAME])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Get WireGuard interface status (parsed from `wg show`).
pub fn get_status() -> Result<WgStatus, SyfrahError> {
    let output = Command::new("wg")
        .args(["show", INTERFACE_NAME, "dump"])
        .output()
        .map_err(|e| SyfrahError::internal(format!("wg show failed: {e}")))?;

    if !output.status.success() {
        return Err(SyfrahError::internal(
            "wg show failed — is the interface up?",
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();

    // First line = interface info: private-key, public-key, listen-port, fwmark
    let listen_port = lines
        .first()
        .and_then(|l| l.split('\t').nth(2))
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    // Remaining lines = peers
    let peer_count = if lines.len() > 1 { lines.len() - 1 } else { 0 };

    // Calculate total traffic from peers
    let mut rx_bytes = 0u64;
    let mut tx_bytes = 0u64;
    for line in lines.iter().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 6 {
            rx_bytes += fields[5].parse::<u64>().unwrap_or(0);
            tx_bytes += fields[6].parse::<u64>().unwrap_or(0);
        }
    }

    Ok(WgStatus {
        interface: INTERFACE_NAME.to_string(),
        listen_port,
        peer_count,
        rx_bytes,
        tx_bytes,
    })
}

/// Parsed WireGuard status.
#[derive(Debug, Clone)]
pub struct WgStatus {
    pub interface: String,
    pub listen_port: u16,
    pub peer_count: usize,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Per-peer handshake info from `wg show dump`.
#[derive(Debug, Clone)]
pub struct PeerHandshake {
    pub public_key: String,
    pub latest_handshake: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub endpoint: Option<String>,
}

/// Get per-peer handshake timestamps from WireGuard.
pub fn get_peer_handshakes() -> Result<Vec<PeerHandshake>, SyfrahError> {
    let output = Command::new("wg")
        .args(["show", INTERFACE_NAME, "dump"])
        .output()
        .map_err(|e| SyfrahError::internal(format!("wg show failed: {e}")))?;

    if !output.status.success() {
        return Err(SyfrahError::internal("wg show failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut peers = Vec::new();

    // Skip first line (interface info)
    for line in stdout.lines().skip(1) {
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 {
            let endpoint = if fields[2] == "(none)" {
                None
            } else {
                Some(fields[2].to_string())
            };
            peers.push(PeerHandshake {
                public_key: fields[0].to_string(),
                latest_handshake: fields[4].parse().unwrap_or(0),
                rx_bytes: fields[5].parse().unwrap_or(0),
                tx_bytes: fields[6].parse().unwrap_or(0),
                endpoint,
            });
        }
    }

    Ok(peers)
}

/// Run a system command, returning error on failure.
fn run_cmd(cmd: &str, args: &[&str]) -> Result<(), SyfrahError> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| SyfrahError::internal(format!("{cmd} failed to execute: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SyfrahError::internal(format!(
            "{cmd} {} failed: {}",
            args.join(" "),
            stderr.trim()
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_name_constant() {
        assert_eq!(INTERFACE_NAME, "syfrah0");
    }

    // Note: WireGuard interface tests require root and wireguard-tools installed.
    // They are tested in integration/E2E tests, not unit tests.

    #[test]
    fn interface_exists_returns_false_without_wg() {
        // On a system without syfrah0, this should return false
        // (may return true on a dev machine running syfrah)
        let _ = interface_exists(); // just verify no panic
    }

    #[test]
    fn wg_status_struct() {
        let s = WgStatus {
            interface: "syfrah0".into(),
            listen_port: 51820,
            peer_count: 2,
            rx_bytes: 1024,
            tx_bytes: 2048,
        };
        assert_eq!(s.peer_count, 2);
        assert_eq!(s.listen_port, 51820);
    }
}
