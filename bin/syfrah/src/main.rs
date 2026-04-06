use anyhow::Result;
use clap::Command;

use syfrah_core::resource::{dispatch, generate_command, ResourceRegistry};

/// Build the resource registry with all known resources.
fn build_registry() -> ResourceRegistry {
    let mut registry = ResourceRegistry::new();

    // Hypervisor — the core resource
    registry.register(syfrah_hypervisor::handlers::registration());

    // Future: vm, vpc, org, subnet, etc.

    registry
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize crypto provider (for TLS in peering)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize structured logging
    let _ = std::fs::create_dir_all("/var/log/syfrah");
    let _guard = {
        use tracing_subscriber::prelude::*;
        use tracing_subscriber::{fmt, EnvFilter};

        let file_appender = tracing_appender::rolling::daily("/var/log/syfrah", "syfrah.log");
        let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

        let subscriber = tracing_subscriber::registry()
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_writer(file_writer)
                    .with_filter(EnvFilter::new("info")),
            )
            .with(
                fmt::layer()
                    .with_target(true)
                    .with_writer(std::io::stderr)
                    .with_filter(EnvFilter::new("warn")),
            );
        tracing::subscriber::set_global_default(subscriber).ok();
        file_guard
    };

    let registry = build_registry();

    let mut app = Command::new("syfrah")
        .about("Syfrah — turn dedicated servers into a programmable cloud")
        .version(env!("CARGO_PKG_VERSION"))
        .subcommand_required(true)
        .arg_required_else_help(true)
        .subcommand(
            Command::new("serve")
                .about("Start the API server")
                .arg(
                    clap::Arg::new("bind")
                        .long("bind")
                        .help("Bind address (default: mesh IPv6 or 0.0.0.0:8443)")
                        .value_name("ADDR"),
                )
                .arg(
                    clap::Arg::new("port")
                        .long("port")
                        .help("Port (default: 8443)")
                        .value_name("PORT"),
                ),
        );

    // Add resource subcommands (hypervisor, future: vm, vpc, etc.)
    for reg in registry.iter() {
        app = app.subcommand(generate_command(&reg.def));
    }

    let matches = app.get_matches();

    match matches.subcommand() {
        Some(("serve", sub_matches)) => {
            serve(sub_matches).await
        }
        Some((sub_name, sub_matches)) => {
            if let Some(reg) = registry.find(sub_name) {
                if let Some((op_name, op_matches)) = sub_matches.subcommand() {
                    dispatch(reg, op_name, op_matches).await
                } else {
                    anyhow::bail!("specify a subcommand. Run 'syfrah {sub_name} --help' for details.");
                }
            } else {
                anyhow::bail!("unknown command: {sub_name}");
            }
        }
        None => {
            anyhow::bail!("specify a command. Run 'syfrah --help' for details.");
        }
    }
}

/// Start the API server.
async fn serve(matches: &clap::ArgMatches) -> Result<()> {
    use syfrah_core::api::{ApiConfig, ApiServer};

    let port: u16 = matches
        .get_one::<String>("port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(8443);

    // Determine bind address: prefer mesh IPv6, fallback to 0.0.0.0
    let bind_addr: std::net::SocketAddr = if let Some(addr) = matches.get_one::<String>("bind") {
        addr.parse()
            .map_err(|_| anyhow::anyhow!("invalid bind address: {addr}"))?
    } else {
        // Try to get mesh IPv6 from fabric state
        let mesh_bind = syfrah_state::LocalDb::open("hypervisor")
            .ok()
            .and_then(|db| {
                syfrah_hypervisor::fabric::state::FabricState::load(&db)
                    .ok()
                    .flatten()
            })
            .map(|state| {
                format!("[{}]:{}", state.hypervisor.mesh_ipv6, port)
                    .parse::<std::net::SocketAddr>()
                    .unwrap_or_else(|_| format!("0.0.0.0:{port}").parse().unwrap())
            })
            .unwrap_or_else(|| format!("0.0.0.0:{port}").parse().unwrap());
        mesh_bind
    };

    let config = ApiConfig {
        admin_addr: bind_addr,
        ..Default::default()
    };

    // Build registry — same handlers as CLI
    let registry = build_registry();
    let registrations: Vec<_> = registry.into_registrations();

    eprintln!("  Starting API server on {bind_addr}");
    eprintln!("  Press Ctrl+C to stop");

    let server = ApiServer::new(config, registrations, vec![]);
    server.run_admin().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use syfrah_core::api::{list_routes, openapi_spec};
    use syfrah_hypervisor::handlers;

    #[test]
    fn api_routes_generated_from_resource_def() {
        let reg = handlers::registration();
        let routes = list_routes(&[reg], "/admin/v1");

        let ops: Vec<&str> = routes.iter().map(|r| r.operation.as_str()).collect();

        assert!(ops.contains(&"init"), "missing init route: {ops:?}");
        assert!(ops.contains(&"join"), "missing join route: {ops:?}");
        assert!(ops.contains(&"status"), "missing status route: {ops:?}");
        assert!(ops.contains(&"list"), "missing list route: {ops:?}");
        assert!(ops.contains(&"get"), "missing get route: {ops:?}");
        assert!(ops.contains(&"leave"), "missing leave route: {ops:?}");
        assert!(ops.contains(&"doctor"), "missing doctor route: {ops:?}");
    }

    #[test]
    fn openapi_spec_generated() {
        let reg = handlers::registration();
        let spec = openapi_spec(&[reg], "/admin/v1");
        assert_eq!(spec["openapi"], "3.0.0");
        assert!(spec["paths"]["/admin/v1/hypervisor"].is_object());
    }

    #[tokio::test]
    async fn api_server_serves_hypervisor_routes() {
        use axum::body::Body;
        use http::Request;
        use syfrah_core::api::ApiConfig;
        use syfrah_core::api::ApiServer;
        use tower::ServiceExt;

        let server = ApiServer::new(ApiConfig::default(), vec![handlers::registration()], vec![]);

        let req = Request::builder()
            .uri("/admin/v1/hypervisor")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router().clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router().clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
