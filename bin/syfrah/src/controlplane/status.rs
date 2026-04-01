//! `syfrah controlplane status` — show enhanced Raft cluster status.

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

    // Also try to query gossip info from members endpoint for member names.
    let members_url = format!("http://[{fabric_ipv6}]:7200/raft/members");
    let members_resp = client.get(&members_url).send().await.ok();
    let _member_names: std::collections::HashMap<u64, String> = if let Some(resp) = members_resp {
        if resp.status().is_success() {
            if let Ok(members) = resp
                .json::<syfrah_controlplane::server::MembersResponse>()
                .await
            {
                members
                    .members
                    .into_iter()
                    .map(|m| (m.node_id, m.addr.clone()))
                    .collect()
            } else {
                std::collections::HashMap::new()
            }
        } else {
            std::collections::HashMap::new()
        }
    } else {
        std::collections::HashMap::new()
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        // Determine if we are the leader.
        let our_role = if status.current_leader == Some(status.id) {
            "Leader"
        } else {
            "Follower"
        };
        let total_members = status.voter_count + status.learner_count;

        println!("Control Plane Status");
        println!("====================");
        println!(
            "  Raft:       {} (term {}, commit {})",
            our_role,
            status.current_term,
            status
                .commit_index
                .map(|i| i.to_string())
                .unwrap_or_else(|| "0".to_string())
        );
        println!(
            "  Members:    {} ({} voter, {} learner)",
            total_members, status.voter_count, status.learner_count
        );

        // Log entries info.
        if let Some(log_entries) = status.log_entries {
            println!("  Log:        {} entries", log_entries);
        }

        // Members table.
        if !status.member_details.is_empty() {
            println!();
            println!("Members:");
            for member in &status.member_details {
                let role_label = if member.is_leader {
                    "Leader"
                } else if member.role == "voter" {
                    "Voter"
                } else {
                    "Learner"
                };

                // Try to extract a readable address — strip brackets and port.
                let addr_display = member.addr.replace("[", "").replace("]", "");
                let addr_clean = addr_display
                    .rsplit_once(':')
                    .map(|(ip, _port)| ip)
                    .unwrap_or(&addr_display);

                // Look up a friendly name (node_id -> name mapping from peers).
                let name = node_id_to_name(member.node_id, &fabric_state);

                println!(
                    "  {:<14} {:<8} {:<10} {}",
                    name, member.role, role_label, addr_clean
                );
            }
        }

        println!();
        println!("Node ID:      {}", status.id);
    }

    Ok(())
}

/// Try to map a Raft node_id back to a human-readable node name.
/// The node_id is derived from `hash(node_name)` during Raft init,
/// so we hash known names and compare.
fn node_id_to_name(node_id: u64, state: &syfrah_fabric::store::NodeState) -> String {
    use std::hash::{Hash, Hasher};

    // Check our own name.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    state.node_name.hash(&mut hasher);
    if hasher.finish() == node_id {
        return state.node_name.clone();
    }

    // Check peer names.
    for peer in &state.peers {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        peer.name.hash(&mut hasher);
        if hasher.finish() == node_id {
            return peer.name.clone();
        }
    }

    // Fallback: show truncated node_id.
    format!("node-{}", node_id % 10000)
}
