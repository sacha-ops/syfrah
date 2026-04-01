use std::collections::HashMap;
use std::fs;
use std::io::Write;

use crate::sanitize::sanitize;
use crate::{no_mesh_error, store};
use anyhow::{Context, Result};
use syfrah_core::mesh::PeerStatus;

const MARKER_BEGIN: &str = "# BEGIN syfrah-fabric";
const MARKER_END: &str = "# END syfrah-fabric";

/// Options for the `hosts` command.
pub struct HostsOpts {
    pub apply: bool,
}

pub async fn run(opts: HostsOpts) -> Result<()> {
    let state = store::load().map_err(|_| no_mesh_error())?;

    let entries = generate_entries(&state);

    if entries.is_empty() {
        eprintln!("No active peers to generate hosts entries for.");
        return Ok(());
    }

    let block = format_block(&entries);

    if opts.apply {
        apply_to_hosts(&block)?;
        eprintln!("Applied {} host entries to /etc/hosts.", entries.len());
    } else {
        print!("{block}");
    }

    Ok(())
}

/// A single hosts entry: IPv6 address -> hostname.
struct HostEntry {
    ip: String,
    name: String,
}

/// Generate host entries from the current node state.
/// Includes the local node and all active peers, deduped by WG public key.
fn generate_entries(state: &store::NodeState) -> Vec<HostEntry> {
    let mut entries = Vec::new();

    // Add the local node itself
    let local_name = sanitize_hostname(&state.node_name);
    if !local_name.is_empty() {
        entries.push(HostEntry {
            ip: state.mesh_ipv6.to_string(),
            name: local_name,
        });
    }

    // Dedup peers by WG public key, keeping the latest last_seen
    let mut by_key: HashMap<&str, &syfrah_core::mesh::PeerRecord> = HashMap::new();
    for peer in &state.peers {
        by_key
            .entry(peer.wg_public_key.as_str())
            .and_modify(|existing| {
                if peer.last_seen > existing.last_seen {
                    *existing = peer;
                }
            })
            .or_insert(peer);
    }

    let mut peers: Vec<&&syfrah_core::mesh::PeerRecord> = by_key.values().collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name));

    for peer in peers {
        if peer.status == PeerStatus::Removed {
            continue;
        }
        let name = sanitize_hostname(&peer.name);
        if name.is_empty() {
            continue;
        }
        entries.push(HostEntry {
            ip: peer.mesh_ipv6.to_string(),
            name,
        });
    }

    entries
}

/// Sanitize a node name into a valid hostname component.
/// Strips control chars, replaces spaces/underscores with hyphens,
/// removes anything that isn't alphanumeric, hyphen, or dot,
/// and lowercases.
fn sanitize_hostname(name: &str) -> String {
    let clean = sanitize(name);
    let hostname: String = clean
        .chars()
        .map(|c| match c {
            ' ' | '_' => '-',
            c if c.is_ascii_alphanumeric() || c == '-' || c == '.' => c,
            _ => '-',
        })
        .collect::<String>()
        .to_ascii_lowercase();
    // Trim leading/trailing hyphens and dots
    hostname
        .trim_matches(|c: char| c == '-' || c == '.')
        .to_string()
}

/// Format a hosts file block with markers.
fn format_block(entries: &[HostEntry]) -> String {
    let mut block = String::new();
    block.push_str(MARKER_BEGIN);
    block.push('\n');
    for entry in entries {
        block.push_str(&format!("{}\t{}\n", entry.ip, entry.name));
    }
    block.push_str(MARKER_END);
    block.push('\n');
    block
}

/// Apply the hosts block to /etc/hosts.
/// Replaces any existing syfrah block (between markers) or appends.
fn apply_to_hosts(block: &str) -> Result<()> {
    let hosts_path = "/etc/hosts";

    let existing = fs::read_to_string(hosts_path)
        .context("Failed to read /etc/hosts. Are you running as root?")?;

    let new_content = replace_or_append(&existing, block);

    let mut file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(hosts_path)
        .context("Failed to open /etc/hosts for writing. Are you running as root?")?;

    file.write_all(new_content.as_bytes())
        .context("Failed to write to /etc/hosts")?;

    Ok(())
}

/// Replace an existing syfrah marker block or append a new one.
fn replace_or_append(existing: &str, block: &str) -> String {
    if let (Some(begin), Some(end)) = (existing.find(MARKER_BEGIN), existing.find(MARKER_END)) {
        // Find the end of the MARKER_END line
        let end_of_marker = existing[end..]
            .find('\n')
            .map(|i| end + i + 1)
            .unwrap_or(existing.len());

        let mut result = String::with_capacity(existing.len());
        result.push_str(&existing[..begin]);
        result.push_str(block);
        result.push_str(&existing[end_of_marker..]);
        result
    } else {
        // Append, ensuring a newline separator
        let mut result = existing.to_string();
        if !result.ends_with('\n') {
            result.push('\n');
        }
        result.push_str(block);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_hostname_normal() {
        assert_eq!(sanitize_hostname("my-node-01"), "my-node-01");
    }

    #[test]
    fn sanitize_hostname_spaces_and_underscores() {
        assert_eq!(sanitize_hostname("My Node_01"), "my-node-01");
    }

    #[test]
    fn sanitize_hostname_special_chars() {
        assert_eq!(sanitize_hostname("node@#$%test"), "node----test");
    }

    #[test]
    fn sanitize_hostname_trim_edges() {
        assert_eq!(sanitize_hostname("-node-"), "node");
        assert_eq!(sanitize_hostname(".node."), "node");
    }

    #[test]
    fn sanitize_hostname_empty() {
        assert_eq!(sanitize_hostname(""), "");
    }

    #[test]
    fn format_block_basic() {
        let entries = vec![
            HostEntry {
                ip: "fd12::1".to_string(),
                name: "node-a".to_string(),
            },
            HostEntry {
                ip: "fd12::2".to_string(),
                name: "node-b".to_string(),
            },
        ];
        let block = format_block(&entries);
        assert!(block.starts_with(MARKER_BEGIN));
        assert!(block.ends_with(&format!("{MARKER_END}\n")));
        assert!(block.contains("fd12::1\tnode-a\n"));
        assert!(block.contains("fd12::2\tnode-b\n"));
    }

    #[test]
    fn replace_or_append_no_existing_block() {
        let existing = "127.0.0.1\tlocalhost\n";
        let block = "# BEGIN syfrah-fabric\nfd12::1\tnode-a\n# END syfrah-fabric\n";
        let result = replace_or_append(existing, block);
        assert_eq!(
            result,
            "127.0.0.1\tlocalhost\n# BEGIN syfrah-fabric\nfd12::1\tnode-a\n# END syfrah-fabric\n"
        );
    }

    #[test]
    fn replace_or_append_replaces_existing_block() {
        let existing = "127.0.0.1\tlocalhost\n# BEGIN syfrah-fabric\nfd12::old\told-node\n# END syfrah-fabric\n::1\tlocalhost\n";
        let block = "# BEGIN syfrah-fabric\nfd12::new\tnew-node\n# END syfrah-fabric\n";
        let result = replace_or_append(existing, block);
        assert_eq!(
            result,
            "127.0.0.1\tlocalhost\n# BEGIN syfrah-fabric\nfd12::new\tnew-node\n# END syfrah-fabric\n::1\tlocalhost\n"
        );
    }

    #[test]
    fn replace_or_append_no_trailing_newline() {
        let existing = "127.0.0.1\tlocalhost";
        let block = "# BEGIN syfrah-fabric\nfd12::1\tnode-a\n# END syfrah-fabric\n";
        let result = replace_or_append(existing, block);
        assert!(result.starts_with("127.0.0.1\tlocalhost\n# BEGIN"));
    }

    #[test]
    fn generate_entries_skips_removed_peers() {
        use std::net::Ipv6Addr;
        let state = store::NodeState {
            mesh_name: "test".into(),
            mesh_secret: "secret".into(),
            wg_private_key: "priv".into(),
            wg_public_key: "pub".into(),
            mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 1),
            mesh_prefix: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 0),
            wg_listen_port: 51820,
            node_name: "local-node".into(),
            public_endpoint: None,
            peering_port: 51821,
            peers: vec![
                syfrah_core::mesh::PeerRecord {
                    name: "peer-a".into(),
                    wg_public_key: "key-a".into(),
                    endpoint: "127.0.0.1:51820".parse().unwrap(),
                    mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 2),
                    last_seen: 100,
                    status: PeerStatus::Active,
                    region: None,
                    zone: None,
                    topology: None,
                    hypervisor_report: None,
                },
                syfrah_core::mesh::PeerRecord {
                    name: "peer-removed".into(),
                    wg_public_key: "key-b".into(),
                    endpoint: "127.0.0.1:51820".parse().unwrap(),
                    mesh_ipv6: Ipv6Addr::new(0xfd12, 0, 0, 0, 0, 0, 0, 3),
                    last_seen: 100,
                    status: PeerStatus::Removed,
                    region: None,
                    zone: None,
                    topology: None,
                    hypervisor_report: None,
                },
            ],
            region: None,
            zone: None,
            metrics: Default::default(),
        };
        let entries = generate_entries(&state);
        // local-node + peer-a, but NOT peer-removed
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "local-node");
        assert_eq!(entries[1].name, "peer-a");
    }
}
