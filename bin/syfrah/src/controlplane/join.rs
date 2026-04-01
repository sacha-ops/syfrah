//! `syfrah controlplane join` — join this node to an existing Raft cluster.

use std::sync::Arc;

use anyhow::{Context, Result};

/// Join an existing Raft cluster by contacting the leader.
pub async fn run() -> Result<()> {
    // Load fabric state to get our node identity and the leader's address.
    let fabric_state = syfrah_fabric::store::load()
        .map_err(|_| anyhow::anyhow!("Fabric not initialized. Run 'syfrah fabric init' first."))?;

    let fabric_ipv6 = fabric_state.mesh_ipv6;

    // Derive node ID from fabric node name hash.
    let node_id = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        fabric_state.node_name.hash(&mut hasher);
        hasher.finish()
    };

    let node_addr = format!("[{fabric_ipv6}]:7200");

    // Check if already initialized.
    let syfrah_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".syfrah");
    let sentinel = syfrah_dir.join("raft_initialized");
    if sentinel.exists() {
        println!("Control plane already initialized on this node.");
        return Ok(());
    }

    // Find a peer that has Raft initialized (the leader).
    // Try each fabric peer's Raft status endpoint.
    let peers = &fabric_state.peers;
    if peers.is_empty() {
        anyhow::bail!("No fabric peers found. Join the fabric first with 'syfrah fabric join'.");
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("Failed to build HTTP client")?;

    let mut leader_addr = None;
    for peer in peers {
        let peer_raft_url = format!("http://[{}]:7200/raft/status", peer.mesh_ipv6);
        if let Ok(resp) = client.get(&peer_raft_url).send().await {
            if resp.status().is_success() {
                if let Ok(status) = resp
                    .json::<syfrah_controlplane::server::RaftStatusResponse>()
                    .await
                {
                    if status.current_leader.is_some() {
                        leader_addr = Some(format!("[{}]:7200", peer.mesh_ipv6));
                        break;
                    }
                }
            }
        }
    }

    let leader_addr = leader_addr.ok_or_else(|| {
        anyhow::anyhow!(
            "No Raft leader found among fabric peers. Initialize the control plane on one node first."
        )
    })?;

    println!("Joining Raft cluster...");
    println!("  Node ID:  {node_id}");
    println!("  Address:  {node_addr}");
    println!("  Leader:   {leader_addr}");

    // Initialize local Raft storage first.
    let log_db =
        syfrah_state::LayerDb::open("raft_log").context("Failed to open raft_log database")?;
    let _log_store = Arc::new(syfrah_controlplane::RedbLogStore::new(log_db));

    // Send join request to the leader.
    let join_url = format!("http://{leader_addr}/raft/join");
    let join_req = serde_json::json!({
        "node_id": node_id,
        "addr": node_addr,
    });

    let resp = client
        .post(&join_url)
        .json(&join_req)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to send join request: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Join failed: {status} — {body}");
    }

    println!("  Joined successfully!");

    // Write sentinel file so the daemon knows to start Raft on next boot.
    std::fs::write(&sentinel, format!("{node_id}"))
        .context("Failed to write raft_initialized sentinel")?;

    println!();
    println!("Control plane joined. Restart the daemon to activate:");
    println!("  syfrah fabric stop && syfrah fabric start");

    Ok(())
}
