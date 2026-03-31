use tokio::process::Command;

use crate::backend::NetworkBackend;
use crate::error::{OverlayError, Result};

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
    async fn run(cmd: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .await
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Err(OverlayError::CommandFailed(format!(
                "{} {} — {}",
                cmd,
                args.join(" "),
                stderr
            )))
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

    async fn create_bridge(&self, name: &str) -> Result<()> {
        if Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "add", name, "type", "bridge"]).await?;
        Self::run("ip", &["link", "set", name, "up"]).await?;
        Ok(())
    }

    async fn add_bridge_ip(&self, bridge: &str, ip: &str, prefix_len: u8) -> Result<()> {
        let cidr = format!("{}/{}", ip, prefix_len);
        if Self::ip_assigned(bridge, &cidr).await {
            return Ok(());
        }
        Self::run("ip", &["addr", "add", &cidr, "dev", bridge]).await?;
        Ok(())
    }

    async fn remove_bridge_ip(&self, bridge: &str, ip: &str) -> Result<()> {
        let output = Command::new("ip")
            .args(["-o", "addr", "show", "dev", bridge])
            .output()
            .await
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);

        for line in stdout.lines() {
            if let Some(pos) = line.find(&format!("inet {}/", ip)) {
                let rest = &line[pos + 5..];
                if let Some(end) = rest.find(' ') {
                    let cidr = &rest[..end];
                    let _ = Self::run("ip", &["addr", "del", cidr, "dev", bridge]).await;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    async fn delete_bridge(&self, name: &str) -> Result<()> {
        if !Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "del", name]).await?;
        Ok(())
    }

    async fn attach_to_bridge(&self, interface: &str, bridge: &str) -> Result<()> {
        let output = Command::new("ip")
            .args(["-o", "link", "show", interface])
            .output()
            .await
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains(&format!("master {}", bridge)) {
            return Ok(());
        }

        Self::run("ip", &["link", "set", interface, "master", bridge]).await?;
        Ok(())
    }

    // ── VXLAN (placeholder — implemented by future PRs) ────────────

    async fn create_vxlan(
        &self,
        _name: &str,
        _vni: u32,
        _local_ip: &str,
        _port: u16,
    ) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "vxlan: not yet implemented".into(),
        ))
    }

    async fn delete_vxlan(&self, _name: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "vxlan: not yet implemented".into(),
        ))
    }

    async fn add_fdb_entry(&self, _bridge: &str, _mac: &str, _vtep: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "fdb: not yet implemented".into(),
        ))
    }

    async fn remove_fdb_entry(&self, _bridge: &str, _mac: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "fdb: not yet implemented".into(),
        ))
    }

    async fn add_arp_proxy(&self, _vxlan: &str, _ip: &str, _mac: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "arp_proxy: not yet implemented".into(),
        ))
    }

    async fn remove_arp_proxy(&self, _vxlan: &str, _ip: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "arp_proxy: not yet implemented".into(),
        ))
    }

    // ── TAP / veth (placeholder) ───────────────────────────────────

    async fn create_tap(&self, _name: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "tap: not yet implemented".into(),
        ))
    }

    async fn delete_tap(&self, _name: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "tap: not yet implemented".into(),
        ))
    }

    async fn create_veth_pair(&self, _name_a: &str, _name_b: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "veth: not yet implemented".into(),
        ))
    }

    // ── Firewall (placeholder) ─────────────────────────────────────

    async fn apply_vm_rules(&self, _tap: &str, _mac: &str, _ip: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    async fn remove_vm_rules(&self, _tap: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    async fn apply_nat(&self, _bridge: &str, _subnet_cidr: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    async fn remove_nat(&self, _bridge: &str, _subnet_cidr: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    async fn apply_peering_rules(&self, _bridge_a: &str, _bridge_b: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    async fn remove_peering_rules(&self, _bridge_a: &str, _bridge_b: &str) -> Result<()> {
        Err(OverlayError::CommandFailed(
            "nft: not yet implemented".into(),
        ))
    }

    // ── Discovery ─────────────────────────────────────────────────

    async fn list_interfaces(&self, prefix: &str) -> Result<Vec<String>> {
        let output = Self::run("ip", &["-o", "link", "show"]).await?;
        let mut result = Vec::new();
        for line in output.lines() {
            // ip -o link output: "N: name: <FLAGS> ..."
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() >= 2 {
                let name = parts[1].trim();
                // Handle "name@if123" for veth peers
                let name = name.split('@').next().unwrap_or(name);
                if name.starts_with(prefix) {
                    result.push(name.to_string());
                }
            }
        }
        Ok(result)
    }
}
