use std::net::{Ipv4Addr, Ipv6Addr};

use tokio::process::Command;

use crate::backend::{BackendError, NetworkBackend};

/// Real Linux implementation using `ip` commands via `tokio::process::Command`.
///
/// All operations are idempotent: creating an existing bridge is a no-op,
/// deleting a missing bridge succeeds silently.
pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }

    /// Run a command, returning Ok(stdout) or Err with stderr.
    async fn run(cmd: &str, args: &[&str]) -> Result<String, BackendError> {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .await
            .map_err(BackendError::Io)?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(BackendError::CommandFailed {
                cmd: format!("{} {}", cmd, args.join(" ")),
                stderr,
            })
        }
    }

    /// Check if a network interface exists.
    async fn interface_exists(name: &str) -> bool {
        Command::new("ip")
            .args(["link", "show", name])
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Check if an IP address is already assigned to a device.
    async fn ip_assigned(device: &str, ip: &str) -> bool {
        Command::new("ip")
            .args(["addr", "show", "dev", device])
            .output()
            .await
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(ip)
            })
            .unwrap_or(false)
    }
}

impl Default for LinuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NetworkBackend for LinuxBackend {
    // ── Bridge ─────────────────────────────────────────────────────

    async fn create_bridge(&self, name: &str) -> Result<(), BackendError> {
        if Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "add", name, "type", "bridge"]).await?;
        Self::run("ip", &["link", "set", name, "up"]).await?;
        Ok(())
    }

    async fn add_bridge_ip(
        &self,
        bridge: &str,
        gateway: Ipv4Addr,
        prefix_len: u8,
    ) -> Result<(), BackendError> {
        let cidr = format!("{}/{}", gateway, prefix_len);
        if Self::ip_assigned(bridge, &cidr).await {
            return Ok(());
        }
        Self::run("ip", &["addr", "add", &cidr, "dev", bridge]).await?;
        Ok(())
    }

    async fn remove_bridge_ip(&self, bridge: &str, gateway: Ipv4Addr) -> Result<(), BackendError> {
        // Find the exact CIDR assigned so we can remove it.
        // If not found, idempotent — succeed silently.
        let output = Command::new("ip")
            .args(["-o", "addr", "show", "dev", bridge])
            .output()
            .await
            .map_err(BackendError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ip_str = gateway.to_string();

        // Find the line containing this IP and extract the full CIDR.
        for line in stdout.lines() {
            if let Some(pos) = line.find(&format!("inet {}/", ip_str)) {
                let rest = &line[pos + 5..]; // skip "inet "
                if let Some(end) = rest.find(' ') {
                    let cidr = &rest[..end];
                    let _ = Self::run("ip", &["addr", "del", cidr, "dev", bridge]).await;
                    return Ok(());
                }
            }
        }
        // IP not found — already removed, idempotent success.
        Ok(())
    }

    async fn delete_bridge(&self, name: &str) -> Result<(), BackendError> {
        if !Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "del", name]).await?;
        Ok(())
    }

    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<(), BackendError> {
        // Check if already attached by reading master.
        let output = Command::new("ip")
            .args(["-o", "link", "show", interface])
            .output()
            .await
            .map_err(BackendError::Io)?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains(&format!("master {}", bridge)) {
            return Ok(());
        }

        Self::run("ip", &["link", "set", interface, "master", bridge]).await?;
        Ok(())
    }

    // ── VXLAN (placeholder — implemented by future issues) ─────────

    async fn create_vxlan(
        &self,
        _name: &str,
        _vni: u32,
        _local_ip: Ipv6Addr,
        _port: u16,
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("vxlan: not yet implemented".into()))
    }

    async fn delete_vxlan(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("vxlan: not yet implemented".into()))
    }

    async fn add_fdb_entry(
        &self,
        _bridge: &str,
        _mac: [u8; 6],
        _vtep: Ipv6Addr,
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("fdb: not yet implemented".into()))
    }

    async fn remove_fdb_entry(&self, _bridge: &str, _mac: [u8; 6]) -> Result<(), BackendError> {
        Err(BackendError::Other("fdb: not yet implemented".into()))
    }

    async fn add_arp_proxy(
        &self,
        _vxlan: &str,
        _ip: Ipv4Addr,
        _mac: [u8; 6],
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("arp_proxy: not yet implemented".into()))
    }

    // ── TAP / veth (placeholder) ───────────────────────────────────

    async fn create_tap(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("tap: not yet implemented".into()))
    }

    async fn delete_tap(&self, _name: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("tap: not yet implemented".into()))
    }

    async fn create_veth_pair(&self, _name_a: &str, _name_b: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("veth: not yet implemented".into()))
    }

    // ── Firewall (placeholder) ─────────────────────────────────────

    async fn apply_vm_rules(
        &self,
        _tap: &str,
        _mac: [u8; 6],
        _ip: Ipv4Addr,
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("nft: not yet implemented".into()))
    }

    async fn remove_vm_rules(&self, _tap: &str) -> Result<(), BackendError> {
        Err(BackendError::Other("nft: not yet implemented".into()))
    }

    async fn apply_nat(
        &self,
        _bridge: &str,
        _subnet: Ipv4Addr,
        _prefix_len: u8,
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("nft: not yet implemented".into()))
    }

    async fn apply_peering_rules(
        &self,
        _bridge_a: &str,
        _bridge_b: &str,
    ) -> Result<(), BackendError> {
        Err(BackendError::Other("nft: not yet implemented".into()))
    }
}
