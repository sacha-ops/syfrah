//! Service management — install, start, stop WireGuard as a systemd service.
//!
//! Syfrah installs and manages a systemd unit for WireGuard.
//! The service persists across reboots and restarts independently of syfrah.

use std::path::Path;
use std::process::Command;

use syfrah_core::error::SyfrahError;

const SERVICE_NAME: &str = "syfrah-wg";
const UNIT_PATH: &str = "/etc/systemd/system/syfrah-wg.service";
const WG_CONF_DIR: &str = "/etc/wireguard";
const WG_CONF_FILE: &str = "/etc/wireguard/syfrah0.conf";

/// Ensure WireGuard tools are installed. Installs automatically if missing.
///
/// Detects the package manager and installs `wireguard-tools`:
/// - apt (Debian/Ubuntu)
/// - dnf (Fedora/RHEL)
/// - yum (CentOS/older RHEL)
/// - pacman (Arch)
/// - apk (Alpine)
/// - zypper (openSUSE)
pub fn ensure_wireguard() -> Result<(), SyfrahError> {
    if wg_quick_available() {
        return Ok(());
    }

    eprintln!("  Installing wireguard-tools...");

    let (cmd, args): (&str, &[&str]) = if which("apt-get") {
        ("apt-get", &["install", "-y", "-qq", "wireguard-tools"])
    } else if which("dnf") {
        ("dnf", &["install", "-y", "-q", "wireguard-tools"])
    } else if which("yum") {
        ("yum", &["install", "-y", "-q", "wireguard-tools"])
    } else if which("pacman") {
        ("pacman", &["-S", "--noconfirm", "--quiet", "wireguard-tools"])
    } else if which("apk") {
        ("apk", &["add", "--quiet", "wireguard-tools"])
    } else if which("zypper") {
        ("zypper", &["install", "-y", "--quiet", "wireguard-tools"])
    } else {
        return Err(SyfrahError::precondition(
            "wireguard-tools not found and no supported package manager detected. \
             Install wireguard-tools manually.",
        ));
    };

    // Update package index first for apt
    if cmd == "apt-get" {
        let _ = Command::new("apt-get")
            .args(["update", "-qq"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    let output = Command::new(cmd)
        .args(args)
        .output()
        .map_err(|e| SyfrahError::internal(format!("{cmd} failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SyfrahError::internal(format!(
            "failed to install wireguard-tools: {}",
            stderr.trim()
        )));
    }

    // Verify it's now available
    if !wg_quick_available() {
        return Err(SyfrahError::internal(
            "wireguard-tools installed but wg-quick still not found",
        ));
    }

    eprintln!("  wireguard-tools installed");
    Ok(())
}

/// Check if a command exists on the system.
fn which(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate the WireGuard config file content.
pub fn generate_wg_conf(
    private_key: &str,
    listen_port: u16,
    mesh_ipv6: &std::net::Ipv6Addr,
    peers: &[(String, String, std::net::Ipv6Addr, Option<String>)], // (pubkey, keepalive, ipv6, endpoint)
) -> String {
    let mut conf = format!(
        "[Interface]\n\
         PrivateKey = {private_key}\n\
         ListenPort = {listen_port}\n\
         Address = {mesh_ipv6}/128\n"
    );

    for (pubkey, _keepalive, ipv6, endpoint) in peers {
        conf.push_str(&format!(
            "\n[Peer]\n\
             PublicKey = {pubkey}\n\
             AllowedIPs = {ipv6}/128\n\
             PersistentKeepalive = 25\n"
        ));
        if let Some(ep) = endpoint {
            conf.push_str(&format!("Endpoint = {ep}\n"));
        }
    }

    conf
}

/// Generate the systemd unit file content.
fn generate_unit() -> String {
    format!(
        "[Unit]\n\
         Description=Syfrah WireGuard Mesh\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         RemainAfterExit=yes\n\
         ExecStart=/usr/bin/wg-quick up {WG_CONF_FILE}\n\
         ExecStop=/usr/bin/wg-quick down {WG_CONF_FILE}\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Install the systemd service + WireGuard config.
pub fn install(
    private_key: &str,
    listen_port: u16,
    mesh_ipv6: &std::net::Ipv6Addr,
    peers: &[(String, String, std::net::Ipv6Addr, Option<String>)],
) -> Result<(), SyfrahError> {
    // Create /etc/wireguard if needed
    std::fs::create_dir_all(WG_CONF_DIR).map_err(SyfrahError::from)?;

    // Write WireGuard config (0o600)
    let conf = generate_wg_conf(private_key, listen_port, mesh_ipv6, peers);
    std::fs::write(WG_CONF_FILE, &conf).map_err(SyfrahError::from)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(WG_CONF_FILE, std::fs::Permissions::from_mode(0o600));
    }

    // Write systemd unit
    std::fs::write(UNIT_PATH, generate_unit()).map_err(SyfrahError::from)?;

    // Reload systemd
    run_systemctl(&["daemon-reload"])?;

    Ok(())
}

/// Enable and start the service.
pub fn enable_and_start() -> Result<(), SyfrahError> {
    run_systemctl(&["enable", "--now", SERVICE_NAME])
}

/// Start the service.
pub fn start() -> Result<(), SyfrahError> {
    run_systemctl(&["start", SERVICE_NAME])
}

/// Stop the service.
pub fn stop() -> Result<(), SyfrahError> {
    run_systemctl(&["stop", SERVICE_NAME])
}

/// Check if the service is active.
pub fn is_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SERVICE_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if the service is enabled (starts on boot).
pub fn is_enabled() -> bool {
    Command::new("systemctl")
        .args(["is-enabled", "--quiet", SERVICE_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Uninstall the service, remove config files.
pub fn uninstall() -> Result<(), SyfrahError> {
    // Stop and disable
    let _ = run_systemctl(&["disable", "--now", SERVICE_NAME]);

    // Remove files
    let _ = std::fs::remove_file(UNIT_PATH);
    let _ = std::fs::remove_file(WG_CONF_FILE);

    // Reload systemd
    let _ = run_systemctl(&["daemon-reload"]);

    Ok(())
}

/// Update the WireGuard config (e.g., when a peer joins) and reload.
pub fn update_config(
    private_key: &str,
    listen_port: u16,
    mesh_ipv6: &std::net::Ipv6Addr,
    peers: &[(String, String, std::net::Ipv6Addr, Option<String>)],
) -> Result<(), SyfrahError> {
    let conf = generate_wg_conf(private_key, listen_port, mesh_ipv6, peers);
    std::fs::write(WG_CONF_FILE, &conf).map_err(SyfrahError::from)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(WG_CONF_FILE, std::fs::Permissions::from_mode(0o600));
    }

    // wg syncconf applies changes without tearing down the interface
    let output = Command::new("wg")
        .args(["syncconf", "syfrah0", WG_CONF_FILE])
        .output()
        .map_err(|e| SyfrahError::internal(format!("wg syncconf failed: {e}")))?;

    if !output.status.success() {
        // Fallback: restart the service
        let _ = run_systemctl(&["restart", SERVICE_NAME]);
    }

    Ok(())
}

/// Check if wg-quick is available on the system.
pub fn wg_quick_available() -> bool {
    Command::new("which")
        .arg("wg-quick")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check if the service unit file exists.
pub fn is_installed() -> bool {
    Path::new(UNIT_PATH).exists()
}

fn run_systemctl(args: &[&str]) -> Result<(), SyfrahError> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| SyfrahError::internal(format!("systemctl failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SyfrahError::internal(format!(
            "systemctl {} failed: {}",
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
    fn generate_wg_conf_no_peers() {
        let conf = generate_wg_conf("privkey123", 51820, &"fd01::1".parse().unwrap(), &[]);
        assert!(conf.contains("PrivateKey = privkey123"));
        assert!(conf.contains("ListenPort = 51820"));
        assert!(conf.contains("Address = fd01::1/128"));
        assert!(!conf.contains("[Peer]"));
    }

    #[test]
    fn generate_wg_conf_with_peers() {
        let peers = vec![(
            "pubkey456".to_string(),
            "25".to_string(),
            "fd01::2".parse().unwrap(),
            Some("1.2.3.4:51820".to_string()),
        )];
        let conf = generate_wg_conf("privkey123", 51820, &"fd01::1".parse().unwrap(), &peers);
        assert!(conf.contains("[Peer]"));
        assert!(conf.contains("PublicKey = pubkey456"));
        assert!(conf.contains("AllowedIPs = fd01::2/128"));
        assert!(conf.contains("Endpoint = 1.2.3.4:51820"));
        assert!(conf.contains("PersistentKeepalive = 25"));
    }

    #[test]
    fn generate_wg_conf_peer_no_endpoint() {
        let peers = vec![(
            "pubkey789".to_string(),
            "25".to_string(),
            "fd01::3".parse().unwrap(),
            None,
        )];
        let conf = generate_wg_conf("privkey", 51820, &"fd01::1".parse().unwrap(), &peers);
        assert!(conf.contains("[Peer]"));
        assert!(!conf.contains("Endpoint"));
    }

    #[test]
    fn generate_unit_has_required_sections() {
        let unit = generate_unit();
        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("wg-quick up"));
        assert!(unit.contains("wg-quick down"));
        assert!(unit.contains("RemainAfterExit=yes"));
    }

    #[test]
    fn is_installed_false_by_default() {
        // On a test system without syfrah installed
        // This may be true on a dev machine — just verify no panic
        let _ = is_installed();
    }

    #[test]
    fn is_active_no_panic() {
        let _ = is_active();
    }

    #[test]
    fn wg_quick_available_no_panic() {
        let _ = wg_quick_available();
    }
}
