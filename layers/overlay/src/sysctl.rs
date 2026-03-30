//! IP forwarding sysctl helper.

use std::fs;
use std::process::Command;

use crate::backend::{BackendError, Result};

/// Path to the kernel parameter for IPv4 forwarding.
const IP_FORWARD_PATH: &str = "/proc/sys/net/ipv4/ip_forward";

/// Ensure `net.ipv4.ip_forward=1` is set.
///
/// Reads the current value first. If already enabled, this is a no-op.
/// If not enabled, sets it via `sysctl`. Logs a warning via `tracing` if
/// the value was previously disabled.
pub fn ensure_ip_forwarding() -> Result<()> {
    let current = fs::read_to_string(IP_FORWARD_PATH)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    if current == "1" {
        return Ok(());
    }

    tracing::warn!("IPv4 forwarding is disabled (net.ipv4.ip_forward={current}), enabling it now");

    let output = Command::new("sysctl")
        .args(["-w", "net.ipv4.ip_forward=1"])
        .output()
        .map_err(BackendError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(BackendError::CommandFailed(format!(
            "failed to enable ip_forward: {stderr}"
        )));
    }

    Ok(())
}
