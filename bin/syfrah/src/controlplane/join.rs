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

    // Use a short timeout for status probes but a longer one for the
    // actual join request (the leader may wait up to 10s for pending
    // membership changes to be applied before promoting).
    let probe_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .context("Failed to build HTTP client")?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("Failed to build HTTP client")?;

    let mut leader_addr = None;
    for peer in peers {
        let peer_raft_url = format!("http://[{}]:7200/raft/status", peer.mesh_ipv6);
        if let Ok(resp) = probe_client.get(&peer_raft_url).send().await {
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

    // Automatically restart the daemon so the Raft server starts immediately.
    // Without this, the daemon's one-shot sentinel check at startup misses
    // the newly created file and Raft never starts until a manual restart.
    println!();
    println!("Restarting daemon to activate Raft...");

    // Stop the running daemon via SIGTERM.
    if let Some(pid) = syfrah_fabric::store::daemon_running() {
        if syfrah_fabric::store::is_syfrah_process(pid) {
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
            // Wait for graceful shutdown (up to 10s).
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if syfrah_fabric::store::daemon_running().is_none() {
                    break;
                }
            }
            if syfrah_fabric::store::daemon_running().is_some() {
                #[cfg(unix)]
                unsafe {
                    libc::kill(pid as i32, libc::SIGKILL);
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            syfrah_fabric::store::remove_pid();
            println!("  Daemon stopped (pid {pid}).");
        }
    }

    // Re-exec the daemon in the background via the syfrah binary.
    // This mirrors the double-fork pattern used by `syfrah fabric start`.
    let exe = std::env::current_exe().context("Failed to find syfrah binary")?;
    let child = std::process::Command::new(&exe)
        .args(["fabric", "start"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    match child {
        Ok(_) => {
            // Wait briefly for the daemon to start and write its PID file.
            for _ in 0..50 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if syfrah_fabric::store::daemon_running().is_some() {
                    break;
                }
            }
            if syfrah_fabric::store::daemon_running().is_some() {
                println!("  Daemon restarted with Raft enabled.");
            } else {
                println!("  Daemon spawned but not yet ready. Check: syfrah fabric status");
            }
        }
        Err(e) => {
            eprintln!("  Warning: failed to restart daemon: {e}");
            println!("  Restart manually: syfrah fabric stop && syfrah fabric start");
        }
    }

    Ok(())
}
