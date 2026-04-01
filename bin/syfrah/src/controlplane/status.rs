//! `syfrah controlplane status` — show Raft cluster status.

use anyhow::{Context, Result};

/// Show the control plane Raft status by querying the local Raft HTTP server.
pub async fn run(json: bool) -> Result<()> {
    // Load fabric state to get our fabric IPv6.
    let fabric_state =
        syfrah_fabric::store::load().map_err(|_| anyhow::anyhow!("Fabric not initialized."))?;

    let fabric_ipv6 = fabric_state.mesh_ipv6;
    let url = format!("http://[{fabric_ipv6}]:7200/raft/status");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client.get(&url).send().await.map_err(|e| {
        anyhow::anyhow!(
            "Cannot reach control plane server at {url}. Is the daemon running?\nError: {e}"
        )
    })?;

    if !resp.status().is_success() {
        anyhow::bail!("Control plane returned status {}", resp.status());
    }

    let status: syfrah_controlplane::server::RaftStatusResponse = resp
        .json()
        .await
        .context("Failed to parse status response")?;

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("Control Plane Status");
        println!("--------------------");
        println!("Node ID:      {}", status.id);
        println!("State:        {}", status.state);
        println!(
            "Leader:       {}",
            status
                .current_leader
                .map(|l| l.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        println!("Term:         {}", status.current_term);
        println!(
            "Last log:     {}",
            status
                .last_log_index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        println!(
            "Last applied: {}",
            status
                .last_applied_index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "none".to_string())
        );
        println!("Members:      {:?}", status.members);
    }

    Ok(())
}
