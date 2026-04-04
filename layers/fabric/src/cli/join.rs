use crate::daemon::{self, DaemonConfig};
use anyhow::Result;
use std::net::SocketAddr;

pub async fn run(
    target: &str,
    node_name: &str,
    port: u16,
    endpoint: Option<SocketAddr>,
    pin: Option<String>,
    region: Option<String>,
    zone: Option<String>,
) -> Result<()> {
    // Parse target: "1.2.3.4" → "1.2.3.4:51821", or "1.2.3.4:9999" as-is
    let target_addr: SocketAddr = if target.contains(':') {
        target
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid target address '{target}': {e}"))?
    } else {
        format!("{target}:51821")
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid target address '{target}': {e}"))?
    };

    daemon::run_join(
        target_addr,
        DaemonConfig {
            mesh_name: String::new(),
            node_name: node_name.to_string(),
            wg_listen_port: port,
            public_endpoint: endpoint,
            peering_port: port + 1,
            region,
            zone,
            no_hypervisor: false,
        },
        pin,
    )
    .await
}
