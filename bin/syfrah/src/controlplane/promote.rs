//! `syfrah controlplane promote/demote` — manage voter/learner roles.

use anyhow::{Context, Result};

/// Resolve a node name to its Raft node_id by hashing.
fn node_name_to_id(name: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut hasher);
    hasher.finish()
}

/// Find the leader address from the local node's Raft status.
async fn find_leader_addr() -> Result<String> {
    let fabric_state =
        syfrah_fabric::store::load().map_err(|_| anyhow::anyhow!("Fabric not initialized."))?;

    let fabric_ipv6 = fabric_state.mesh_ipv6;
    let url = format!("http://[{fabric_ipv6}]:7200/raft/status");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .context("Failed to build HTTP client")?;

    let resp = client.get(&url).send().await.map_err(|e| {
        anyhow::anyhow!("Cannot reach control plane. Is the daemon running?\nError: {e}")
    })?;

    if !resp.status().is_success() {
        anyhow::bail!("Control plane returned status {}", resp.status());
    }

    let status: syfrah_controlplane::server::RaftStatusResponse =
        resp.json().await.context("Failed to parse status")?;

    // Find the leader's address from member details.
    for member in &status.member_details {
        if member.is_leader {
            return Ok(member.addr.clone());
        }
    }

    // Fallback: use our own address if we are the leader.
    if status.current_leader == Some(status.id) {
        return Ok(format!("[{fabric_ipv6}]:7200"));
    }

    anyhow::bail!("No leader found in cluster. Cluster may be electing.")
}

/// Promote a learner to voter.
pub async fn run_promote(node_name: &str) -> Result<()> {
    let node_id = node_name_to_id(node_name);
    let leader_addr = find_leader_addr().await?;

    println!("Promoting {node_name} (node_id={node_id}) to voter...");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;

    let url = format!("http://{leader_addr}/raft/promote");
    let body = serde_json::json!({ "node_id": node_id });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Promote request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Promote failed: {status} — {text}");
    }

    println!("{node_name} promoted to voter successfully.");
    Ok(())
}

/// Demote a voter to learner.
pub async fn run_demote(node_name: &str) -> Result<()> {
    let node_id = node_name_to_id(node_name);
    let leader_addr = find_leader_addr().await?;

    println!("Demoting {node_name} (node_id={node_id}) to learner...");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("Failed to build HTTP client")?;

    let url = format!("http://{leader_addr}/raft/demote");
    let body = serde_json::json!({ "node_id": node_id });

    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Demote request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Demote failed: {status} — {text}");
    }

    println!("{node_name} demoted to learner successfully.");
    Ok(())
}
