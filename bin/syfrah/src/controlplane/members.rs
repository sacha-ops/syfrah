//! `syfrah controlplane members` — list all Raft cluster members.

use anyhow::{Context, Result};

/// List all members of the Raft cluster with their roles.
pub async fn run() -> Result<()> {
    let fabric_state =
        syfrah_fabric::store::load().map_err(|_| anyhow::anyhow!("Fabric not initialized."))?;

    let fabric_ipv6 = fabric_state.mesh_ipv6;
    let url = format!("http://[{fabric_ipv6}]:7200/raft/members");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client.get(&url).send().await.map_err(|e| {
        anyhow::anyhow!("Cannot reach control plane server. Is the daemon running?\nError: {e}")
    })?;

    if !resp.status().is_success() {
        anyhow::bail!("Control plane returned status {}", resp.status());
    }

    let members: syfrah_controlplane::server::MembersResponse = resp
        .json()
        .await
        .context("Failed to parse members response")?;

    if members.members.is_empty() {
        println!("(no members)");
        return Ok(());
    }

    println!("{:<20} {:<40} {:<10}", "NODE ID", "ADDRESS", "ROLE");
    println!("{}", "-".repeat(70));
    for m in &members.members {
        println!("{:<20} {:<40} {:<10}", m.node_id, m.addr, m.role);
    }
    println!();
    println!("{} member(s) total", members.members.len());

    Ok(())
}
