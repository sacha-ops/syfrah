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

    // ── VXLAN ───────────────────────────────────────────────────────

    async fn create_vxlan(&self, name: &str, vni: u32, local_ip: &str, port: u16) -> Result<()> {
        if Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run(
            "ip",
            &[
                "link",
                "add",
                name,
                "type",
                "vxlan",
                "id",
                &vni.to_string(),
                "local",
                local_ip,
                "dstport",
                &port.to_string(),
                "nolearning",
                "proxy",
            ],
        )
        .await?;
        Self::run("ip", &["link", "set", name, "up"]).await?;
        // Enable ARP proxy on the VXLAN interface so the kernel responds to
        // ARP requests using entries from the neighbor table (populated by
        // `ip neigh replace`). Without this, remote VM ARPs go unanswered.
        Self::run(
            "sysctl",
            &["-w", &format!("net.ipv4.conf.{name}.proxy_arp=1")],
        )
        .await
        .ok();
        Ok(())
    }

    async fn delete_vxlan(&self, name: &str) -> Result<()> {
        if !Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "del", name]).await?;
        Ok(())
    }

    async fn add_fdb_entry(&self, bridge: &str, mac: &str, vtep: &str) -> Result<()> {
        let vxlan = bridge.replace(crate::naming::BRIDGE_PREFIX, crate::naming::VXLAN_PREFIX);
        Self::run("bridge", &["fdb", "add", mac, "dev", &vxlan, "dst", vtep]).await?;
        Ok(())
    }

    async fn remove_fdb_entry(&self, bridge: &str, mac: &str) -> Result<()> {
        let vxlan = bridge.replace(crate::naming::BRIDGE_PREFIX, crate::naming::VXLAN_PREFIX);
        Self::run("bridge", &["fdb", "del", mac, "dev", &vxlan]).await?;
        Ok(())
    }

    async fn add_arp_proxy(&self, vxlan: &str, ip: &str, mac: &str) -> Result<()> {
        Self::run(
            "ip",
            &[
                "neigh",
                "replace",
                ip,
                "lladdr",
                mac,
                "dev",
                vxlan,
                "nud",
                "permanent",
            ],
        )
        .await?;
        Ok(())
    }

    async fn remove_arp_proxy(&self, vxlan: &str, ip: &str) -> Result<()> {
        Self::run("ip", &["neigh", "del", ip, "dev", vxlan]).await?;
        Ok(())
    }

    // ── TAP / veth ─────────────────────────────────────────────────

    async fn create_tap(&self, name: &str) -> Result<()> {
        if Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["tuntap", "add", "dev", name, "mode", "tap"]).await?;
        Self::run("ip", &["link", "set", name, "up"]).await?;
        Ok(())
    }

    async fn delete_tap(&self, name: &str) -> Result<()> {
        if !Self::interface_exists(name).await {
            return Ok(());
        }
        Self::run("ip", &["link", "del", name]).await?;
        Ok(())
    }

    async fn create_veth_pair(&self, name_a: &str, name_b: &str) -> Result<()> {
        if Self::interface_exists(name_a).await {
            return Ok(());
        }
        Self::run(
            "ip",
            &[
                "link", "add", name_a, "type", "veth", "peer", "name", name_b,
            ],
        )
        .await?;

        // Wait for the kernel to make the devices visible. The netlink
        // creation is asynchronous — the device may not appear in sysfs
        // immediately. Retry up to 3 times with 100ms delay.
        for attempt in 1..=3 {
            if Self::interface_exists(name_a).await && Self::interface_exists(name_b).await {
                break;
            }
            tracing::debug!(
                attempt,
                name_a,
                name_b,
                "veth pair not yet visible, waiting 100ms"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        Self::run("ip", &["link", "set", name_a, "up"]).await?;
        Self::run("ip", &["link", "set", name_b, "up"]).await?;
        Ok(())
    }

    async fn move_to_netns(&self, iface: &str, pid: u32) -> Result<()> {
        // Retry up to 3 times if the interface is not yet visible (race
        // condition between veth creation and netns move).
        for attempt in 1..=3 {
            if Self::interface_exists(iface).await {
                Self::run("ip", &["link", "set", iface, "netns", &pid.to_string()]).await?;
                return Ok(());
            }
            tracing::debug!(
                attempt,
                iface,
                "interface not yet visible for netns move, waiting 100ms"
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Final attempt without existence check — let it fail with a proper error.
        Self::run("ip", &["link", "set", iface, "netns", &pid.to_string()]).await?;
        Ok(())
    }

    async fn configure_netns(
        &self,
        pid: u32,
        iface: &str,
        ip: &str,
        prefix_len: u8,
        gateway: &str,
        mac: &str,
    ) -> Result<()> {
        // Use --net=<path> (not --net <path>) for compatibility with
        // util-linux nsenter which requires the = form.
        let ns_flag = format!("--net=/proc/{pid}/ns/net");
        let cidr = format!("{ip}/{prefix_len}");

        // Rename the veth endpoint to eth0 inside the namespace.
        Self::run(
            "nsenter",
            &[&ns_flag, "ip", "link", "set", iface, "name", "eth0"],
        )
        .await?;
        // Set MAC address to match IPAM-derived MAC (required for anti-spoofing)
        Self::run(
            "nsenter",
            &[&ns_flag, "ip", "link", "set", "eth0", "address", mac],
        )
        .await?;
        // Assign the allocated IP.
        Self::run(
            "nsenter",
            &[&ns_flag, "ip", "addr", "add", &cidr, "dev", "eth0"],
        )
        .await?;
        // Bring up eth0 and loopback.
        Self::run("nsenter", &[&ns_flag, "ip", "link", "set", "eth0", "up"]).await?;
        Self::run("nsenter", &[&ns_flag, "ip", "link", "set", "lo", "up"]).await?;
        // Default route through the bridge gateway.
        Self::run(
            "nsenter",
            &[&ns_flag, "ip", "route", "add", "default", "via", gateway],
        )
        .await?;
        Ok(())
    }

    // ── Firewall ───────────────────────────────────────────────────

    async fn enable_br_netfilter(&self) -> Result<()> {
        // Load the module — ignore error if already loaded or built-in.
        let _ = std::process::Command::new("modprobe")
            .arg("br_netfilter")
            .output();

        // Enable nftables/iptables hooks on bridged packets so that the
        // forward chain sees VM-to-VM traffic crossing the same bridge.
        for path in &[
            "/proc/sys/net/bridge/bridge-nf-call-iptables",
            "/proc/sys/net/bridge/bridge-nf-call-ip6tables",
        ] {
            let _ = std::fs::write(path, "1");
        }
        Ok(())
    }

    async fn apply_infra_protection(&self) -> Result<()> {
        let ruleset = crate::nft::generate_infra_protection();
        crate::nft::apply_ruleset(&ruleset)
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;
        Ok(())
    }

    async fn apply_sg_base_chain(&self) -> Result<()> {
        let ruleset = crate::sg_nft::build_sg_base_chain();
        crate::nft::apply_ruleset(&ruleset)
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;
        Ok(())
    }

    async fn apply_bridge_accept_rules(&self, bridge: &str) -> Result<()> {
        let ruleset = crate::nft::generate_bridge_accept_rules(bridge);
        crate::nft::apply_ruleset(&ruleset)
            .map_err(|e| OverlayError::CommandFailed(e.to_string()))?;
        Ok(())
    }

    async fn apply_vm_rules(&self, tap: &str, mac: &str, ip: &str) -> Result<()> {
        // Ensure table and chain exist (ignore errors if already present)
        Self::run("nft", &["add", "table", "inet", "syfrah"])
            .await
            .ok();
        Self::run(
            "nft",
            &[
                "add",
                "chain",
                "inet",
                "syfrah",
                "forward",
                "{ type filter hook forward priority 0; policy drop; }",
            ],
        )
        .await
        .ok();

        // Anti-spoofing: drop packets from this TAP with wrong MAC or IP
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "iif", tap, "ether", "saddr", "!=",
                mac, "drop",
            ],
        )
        .await?;
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "iif", tap, "ip", "saddr", "!=", ip,
                "drop",
            ],
        )
        .await?;

        // Ingress rules: conntrack, SSH, ICMP, default deny
        Self::run(
            "nft",
            &[
                "add",
                "rule",
                "inet",
                "syfrah",
                "forward",
                "oif",
                tap,
                "ct",
                "state",
                "established,related",
                "accept",
            ],
        )
        .await?;
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "oif", tap, "tcp", "dport", "22",
                "accept",
            ],
        )
        .await?;
        Self::run(
            "nft",
            &[
                "add",
                "rule",
                "inet",
                "syfrah",
                "forward",
                "oif",
                tap,
                "icmp",
                "type",
                "echo-request",
                "accept",
            ],
        )
        .await?;
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "oif", tap, "drop",
            ],
        )
        .await?;

        Ok(())
    }

    async fn remove_vm_rules(&self, tap: &str) -> Result<()> {
        // List all rules with handles, then delete those matching this TAP
        let output = Self::run("nft", &["-a", "list", "chain", "inet", "syfrah", "forward"])
            .await
            .unwrap_or_default();

        for line in output.lines() {
            let trimmed = line.trim();
            if !trimmed.contains(tap) {
                continue;
            }
            // Lines look like: "iif "syft-xxx" ... # handle 42"
            if let Some(handle_pos) = trimmed.rfind("# handle ") {
                let handle = trimmed[handle_pos + 9..].trim();
                Self::run(
                    "nft",
                    &[
                        "delete", "rule", "inet", "syfrah", "forward", "handle", handle,
                    ],
                )
                .await
                .ok();
            }
        }
        Ok(())
    }

    async fn apply_nat(&self, _bridge: &str, subnet_cidr: &str) -> Result<()> {
        Self::run("nft", &["add", "table", "ip", "syfrah_nat"])
            .await
            .ok();
        Self::run(
            "nft",
            &[
                "add",
                "chain",
                "ip",
                "syfrah_nat",
                "postrouting",
                "{ type nat hook postrouting priority 100; }",
            ],
        )
        .await
        .ok();
        Self::run(
            "nft",
            &[
                "add",
                "rule",
                "ip",
                "syfrah_nat",
                "postrouting",
                "ip",
                "saddr",
                subnet_cidr,
                "masquerade",
            ],
        )
        .await?;
        // Enable IP forwarding
        Self::run("sysctl", &["-w", "net.ipv4.ip_forward=1"])
            .await
            .ok();
        Ok(())
    }

    async fn remove_nat(&self, _bridge: &str, subnet_cidr: &str) -> Result<()> {
        let output = Self::run(
            "nft",
            &["-a", "list", "chain", "ip", "syfrah_nat", "postrouting"],
        )
        .await
        .unwrap_or_default();

        for line in output.lines() {
            let trimmed = line.trim();
            if !trimmed.contains(subnet_cidr) {
                continue;
            }
            if let Some(handle_pos) = trimmed.rfind("# handle ") {
                let handle = trimmed[handle_pos + 9..].trim();
                Self::run(
                    "nft",
                    &[
                        "delete",
                        "rule",
                        "ip",
                        "syfrah_nat",
                        "postrouting",
                        "handle",
                        handle,
                    ],
                )
                .await
                .ok();
            }
        }
        Ok(())
    }

    async fn apply_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        Self::run("nft", &["add", "table", "inet", "syfrah"])
            .await
            .ok();
        Self::run(
            "nft",
            &[
                "add",
                "chain",
                "inet",
                "syfrah",
                "forward",
                "{ type filter hook forward priority 0; policy drop; }",
            ],
        )
        .await
        .ok();

        // Allow forwarding in both directions between the peered bridges
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "iif", bridge_a, "oif", bridge_b,
                "accept",
            ],
        )
        .await?;
        Self::run(
            "nft",
            &[
                "add", "rule", "inet", "syfrah", "forward", "iif", bridge_b, "oif", bridge_a,
                "accept",
            ],
        )
        .await?;
        Ok(())
    }

    async fn remove_peering_rules(&self, bridge_a: &str, bridge_b: &str) -> Result<()> {
        let output = Self::run("nft", &["-a", "list", "chain", "inet", "syfrah", "forward"])
            .await
            .unwrap_or_default();

        for line in output.lines() {
            let trimmed = line.trim();
            // Match rules that reference both bridges (peering rules)
            if !(trimmed.contains(bridge_a) && trimmed.contains(bridge_b)) {
                continue;
            }
            if let Some(handle_pos) = trimmed.rfind("# handle ") {
                let handle = trimmed[handle_pos + 9..].trim();
                Self::run(
                    "nft",
                    &[
                        "delete", "rule", "inet", "syfrah", "forward", "handle", handle,
                    ],
                )
                .await
                .ok();
            }
        }
        Ok(())
    }

    async fn link_exists(&self, name: &str) -> bool {
        Self::interface_exists(name).await
    }

    async fn list_interfaces(&self, prefix: &str) -> Result<Vec<String>> {
        let output = Self::run("ip", &["-o", "link", "show"]).await?;
        let mut names = Vec::new();
        for line in output.lines() {
            // Format: "2: eth0: <...>"
            if let Some(name_part) = line.split(':').nth(1) {
                let name = name_part.trim().split('@').next().unwrap_or("").trim();
                if name.starts_with(prefix) {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    async fn list_fdb_entries(&self, vxlan: &str) -> Result<Vec<(String, String)>> {
        let output = Self::run("bridge", &["fdb", "show", "dev", vxlan]).await?;
        let mut entries = Vec::new();
        for line in output.lines() {
            // Format: "02:00:0a:01:00:03 dst fd12::1 self permanent"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[1] == "dst" {
                let mac = parts[0].to_string();
                let dst = parts[2].to_string();
                entries.push((mac, dst));
            }
        }
        Ok(entries)
    }

    async fn list_arp_entries(&self, vxlan: &str) -> Result<Vec<(String, String)>> {
        let output = Self::run("ip", &["neigh", "show", "dev", vxlan, "nud", "permanent"]).await?;
        let mut entries = Vec::new();
        for line in output.lines() {
            // Format: "10.1.0.3 lladdr 02:00:0a:01:00:03 PERMANENT"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 && parts[1] == "lladdr" {
                let ip = parts[0].to_string();
                let mac = parts[2].to_string();
                entries.push((ip, mac));
            }
        }
        Ok(entries)
    }
}
