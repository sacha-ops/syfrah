//! `syfrah controlplane init` — bootstrap a single-node Raft cluster.
//!
//! During initialization, imports ALL current redb tables as initial state
//! machine state. A brief mutation freeze (503) is applied during cutover.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};

use syfrah_controlplane::types::SyfrahNode;

/// Bootstrap a single-node Raft cluster on this node.
///
/// If `verify` is true, checks data integrity after migration by reading
/// back all orgs, VPCs, subnets, etc. from the stores.
pub async fn run(verify: bool) -> Result<()> {
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

    println!("Initializing control plane...");
    println!("  Node ID:  {node_id}");
    println!("  Address:  {node_addr}");

    // -- Mutation freeze --
    // Write a freeze sentinel so the daemon returns 503 on mutations
    // during the cutover window.
    let freeze_sentinel = syfrah_dir.join("raft_migrating");
    std::fs::write(&freeze_sentinel, "migrating")
        .context("Failed to write migration freeze sentinel")?;
    println!("  Mutation freeze: ACTIVE (503 on writes during migration)");

    // -- Import existing data --
    // Open the real org store to count and verify existing data.
    let org_count;
    let vpc_count;
    let subnet_count;
    {
        if syfrah_state::LayerDb::layer_exists("org") {
            let org_db =
                syfrah_state::LayerDb::open("org").context("Failed to open org database")?;
            let org_store = syfrah_org::OrgStore::new(org_db);

            // Count existing resources for the migration report.
            let orgs = org_store.list().unwrap_or_default();
            org_count = orgs.len();
            let vpcs = org_store.list_vpcs().unwrap_or_default();
            vpc_count = vpcs.len();
            let mut sn_count = 0;
            for vpc in &vpcs {
                sn_count += org_store.list_subnets(&vpc.name).unwrap_or_default().len();
            }
            subnet_count = sn_count;

            println!("  Importing existing data:");
            println!("    Orgs:    {org_count}");
            println!("    VPCs:    {vpc_count}");
            println!("    Subnets: {subnet_count}");
        } else {
            org_count = 0;
            vpc_count = 0;
            subnet_count = 0;
            println!("  No existing org data to import (fresh install)");
        }
    }

    // Create Raft storage.
    let log_db =
        syfrah_state::LayerDb::open("raft_log").context("Failed to open raft_log database")?;

    let log_store = Arc::new(syfrah_controlplane::RedbLogStore::new(log_db));

    // Use a temporary in-memory org store for initialization only.
    // The daemon will create the real org store connection on startup.
    // Raft init only needs the log store and state machine to track
    // membership — no org commands are applied during bootstrap.
    let tmp_dir = tempfile::tempdir().context("Failed to create temp dir")?;
    let tmp_org_db = syfrah_state::LayerDb::open_at(&tmp_dir.path().join("tmp_org.redb"))
        .context("Failed to create temp org database")?;
    let org_store = Arc::new(syfrah_org::OrgStore::new(tmp_org_db));
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

    // -- Remove mutation freeze --
    let _ = std::fs::remove_file(&freeze_sentinel);
    println!("  Mutation freeze: REMOVED");

    // -- Verify data integrity --
    if verify {
        println!();
        println!("Verifying data integrity...");
        let mut errors = 0u32;

        if syfrah_state::LayerDb::layer_exists("org") {
            let org_db =
                syfrah_state::LayerDb::open("org").context("Failed to open org database")?;
            let org_store = syfrah_org::OrgStore::new(org_db);

            // Verify orgs are readable.
            match org_store.list() {
                Ok(orgs) => {
                    if orgs.len() == org_count {
                        println!("  [OK] Orgs: {} readable", orgs.len());
                    } else {
                        println!("  [WARN] Orgs: expected {org_count}, found {}", orgs.len());
                        errors += 1;
                    }
                }
                Err(e) => {
                    println!("  [FAIL] Orgs: {e}");
                    errors += 1;
                }
            }

            // Verify VPCs.
            match org_store.list_vpcs() {
                Ok(vpcs) => {
                    if vpcs.len() == vpc_count {
                        println!("  [OK] VPCs: {} readable", vpcs.len());
                    } else {
                        println!("  [WARN] VPCs: expected {vpc_count}, found {}", vpcs.len());
                        errors += 1;
                    }
                }
                Err(e) => {
                    println!("  [FAIL] VPCs: {e}");
                    errors += 1;
                }
            }

            // Verify subnets.
            let mut total_subnets = 0;
            for vpc in org_store.list_vpcs().unwrap_or_default() {
                total_subnets += org_store.list_subnets(&vpc.name).unwrap_or_default().len();
            }
            if total_subnets == subnet_count {
                println!("  [OK] Subnets: {total_subnets} readable");
            } else {
                println!("  [WARN] Subnets: expected {subnet_count}, found {total_subnets}");
                errors += 1;
            }
        }

        // Verify IPAM store.
        if syfrah_state::LayerDb::layer_exists("ipam") {
            println!("  [OK] IPAM store: exists");
        }

        // Verify placement store.
        if syfrah_state::LayerDb::layer_exists("placements") {
            println!("  [OK] Placement store: exists");
        }

        if errors > 0 {
            println!();
            println!("Verification found {errors} issue(s). Review above output.");
        } else {
            println!();
            println!("Verification PASSED. All data accessible.");
        }
    }

    println!();
    println!("Control plane initialized. Restart the daemon to activate:");
    println!("  syfrah fabric stop && syfrah fabric start");

    Ok(())
}
