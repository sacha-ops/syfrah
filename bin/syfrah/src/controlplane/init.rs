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

    let fabric_ipv6 = fabric_state.mesh_ipv6;

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

    // Create storage with a temporary org store (just for initialization).
    // The daemon will create the real org store connection on startup.
    let log_store = Arc::new(syfrah_controlplane::RedbLogStore::new(log_db));

    // Use a temporary in-memory org store for initialization only.
    // The real org store is locked by the daemon. Raft init only needs
    // the log store and state machine to track membership — no org commands
    // are applied during bootstrap.
    let tmp_dir = tempfile::tempdir().context("Failed to create temp dir")?;
    let tmp_org_db = syfrah_state::LayerDb::open_at(&tmp_dir.path().join("tmp_org.redb"))
        .context("Failed to create temp org database")?;
    let org_store = Arc::new(syfrah_org::OrgStore::new(tmp_org_db));
    let sm = Arc::new(syfrah_controlplane::RedbStateMachine::new(org_store));

    let network = syfrah_controlplane::SyfrahNetworkFactory::new();

    let config = Arc::new(openraft::Config {
        cluster_name: "syfrah-raft".to_string(),
        snapshot_policy: openraft::SnapshotPolicy::LogsSinceLast(
            syfrah_controlplane::state_machine::DEFAULT_SNAPSHOT_THRESHOLD,
        ),
        max_in_snapshot_log_to_keep: 100,
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
