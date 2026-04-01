//! `syfrah controlplane init` — bootstrap a single-node Raft cluster.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use syfrah_controlplane::types::SyfrahNode;

/// Bootstrap a single-node Raft cluster on this node.
pub async fn run() -> Result<()> {
    // Load fabric state to get our node identity.
    let fabric_state = syfrah_fabric::store::load()
        .map_err(|_| anyhow::anyhow!("Fabric not initialized. Run 'syfrah fabric init' first."))?;

    let my_record = fabric_state
        .peers
        .iter()
        .find(|p| p.name == fabric_state.node_name)
        .ok_or_else(|| anyhow::anyhow!("Cannot find own peer record in fabric state"))?;

    let fabric_ipv6 = my_record.mesh_ipv6;

    // Derive node ID from fabric node name hash.
    let node_id = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        fabric_state.node_name.hash(&mut hasher);
        hasher.finish()
    };

    let node_addr = format!("[{fabric_ipv6}]:7200");

    // Check if already initialized via sentinel file.
    let syfrah_dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".syfrah");
    let sentinel = syfrah_dir.join("raft_initialized");
    if sentinel.exists() {
        println!("Control plane already initialized. Restart the daemon to activate.");
        return Ok(());
    }

    let log_db =
        syfrah_state::LayerDb::open("raft_log").context("Failed to open raft_log database")?;

    println!("Initializing control plane...");
    println!("  Node ID:  {node_id}");
    println!("  Address:  {node_addr}");

    // Create storage and state machine.
    let log_store = Arc::new(syfrah_controlplane::RedbLogStore::new(log_db));

    let org_db = syfrah_state::LayerDb::open("org").context("Failed to open org database")?;
    let org_store = Arc::new(syfrah_org::OrgStore::new(org_db));
    let sm = Arc::new(syfrah_controlplane::RedbStateMachine::new(org_store));

    let network = syfrah_controlplane::SyfrahNetworkFactory::new();

    let config = Arc::new(openraft::Config {
        cluster_name: "syfrah-raft".to_string(),
        ..Default::default()
    });

    // Create the Raft node.
    let raft = openraft::Raft::new(node_id, config, network, log_store, sm)
        .await
        .context("Failed to create Raft node")?;

    // Initialize as a single-member cluster.
    let mut members = BTreeMap::new();
    members.insert(
        node_id,
        SyfrahNode {
            addr: node_addr.clone(),
        },
    );

    raft.initialize(members)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to initialize Raft cluster: {e}"))?;

    println!("  Raft cluster initialized (node_id={node_id})");

    // Wait briefly for the node to become leader.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Check metrics to confirm leadership.
    {
        use openraft::rt::watch::WatchReceiver;
        let metrics = raft.metrics().borrow_watched().clone();
        println!("  State:    {:?}", metrics.state);
        println!("  Leader:   {:?}", metrics.current_leader);
        println!("  Term:     {}", metrics.current_term);
    }

    // Shut down the temporary Raft node — the daemon will start a new one.
    raft.shutdown()
        .await
        .map_err(|e| anyhow::anyhow!("Raft shutdown error: {e}"))?;

    // Write sentinel file so the daemon knows to start Raft on next boot.
    std::fs::write(&sentinel, format!("{node_id}"))
        .context("Failed to write raft_initialized sentinel")?;

    println!();
    println!("Control plane initialized. Restart the daemon to activate:");
    println!("  syfrah fabric stop && syfrah fabric start");

    Ok(())
}
