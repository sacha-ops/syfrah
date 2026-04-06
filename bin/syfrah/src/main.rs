use anyhow::Result;
use clap::Command;

use syfrah_core::resource::{dispatch, generate_command, ResourceRegistry};

/// Build the resource registry with all known resources.
fn build_registry() -> ResourceRegistry {
    let mut registry = ResourceRegistry::new();

    // Hypervisor — the core resource
    registry.register(syfrah_hypervisor::handlers::registration());

    registry
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize crypto provider (for TLS in peering)
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Initialize structured logging
    // File: info level (for debugging), stderr: warn level (clean UX)
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
        .arg_required_else_help(true);

    for reg in registry.iter() {
        app = app.subcommand(generate_command(&reg.def));
    }

    let matches = app.get_matches();

    if let Some((sub_name, sub_matches)) = matches.subcommand() {
        if let Some(reg) = registry.find(sub_name) {
            if let Some((op_name, op_matches)) = sub_matches.subcommand() {
                return dispatch(reg, op_name, op_matches).await;
            } else {
                anyhow::bail!("specify a subcommand. Run 'syfrah {sub_name} --help' for details.");
            }
        } else {
            anyhow::bail!("unknown command: {sub_name}");
        }
    }

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

        // Should have routes for all operations
        let _paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        let ops: Vec<&str> = routes.iter().map(|r| r.operation.as_str()).collect();

        assert!(ops.contains(&"init"), "missing init route: {ops:?}");
        assert!(ops.contains(&"join"), "missing join route: {ops:?}");
        assert!(ops.contains(&"status"), "missing status route: {ops:?}");
        assert!(ops.contains(&"list"), "missing list route: {ops:?}");
        assert!(ops.contains(&"get"), "missing get route: {ops:?}");
        assert!(ops.contains(&"leave"), "missing leave route: {ops:?}");
        assert!(ops.contains(&"drain"), "missing drain route: {ops:?}");
        assert!(ops.contains(&"enable"), "missing enable route: {ops:?}");

        // Check REST methods
        assert!(
            routes
                .iter()
                .any(|r| r.method == "GET" && r.path == "/admin/v1/hypervisor"),
            "missing GET list"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.method == "GET" && r.path.contains("{id}")),
            "missing GET by id"
        );
        assert!(
            routes
                .iter()
                .any(|r| r.method == "POST" && r.path.contains("init")),
            "missing POST init"
        );

        println!("\nGenerated API routes:");
        for r in &routes {
            println!("  {} {:<40} {}", r.method, r.path, r.operation);
        }
    }

    #[test]
    fn openapi_spec_generated() {
        let reg = handlers::registration();
        let spec = openapi_spec(&[reg], "/admin/v1");

        assert_eq!(spec["openapi"], "3.0.0");
        assert!(spec["paths"]["/admin/v1/hypervisor"].is_object());
        assert!(spec["paths"]["/admin/v1/hypervisor/{id}"].is_object());
        assert!(spec["paths"]["/admin/v1/hypervisor/init"].is_object());

        println!("\nOpenAPI spec:");
        println!("{}", serde_json::to_string_pretty(&spec).unwrap());
    }

    #[tokio::test]
    async fn api_server_serves_hypervisor_routes() {
        use axum::body::Body;
        use http::Request;
        use syfrah_core::api::ApiConfig;
        use syfrah_core::api::ApiServer;
        use tower::ServiceExt;

        let server = ApiServer::new(ApiConfig::default(), vec![handlers::registration()], vec![]);

        // GET /admin/v1/hypervisor → list (returns empty since no state)
        let req = Request::builder()
            .uri("/admin/v1/hypervisor")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router().clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200, "list endpoint should work");

        // POST /admin/v1/hypervisor/status → status
        let req = Request::builder()
            .method("POST")
            .uri("/admin/v1/hypervisor/status")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = server.admin_router().clone().oneshot(req).await.unwrap();
        // May return 500 (no state) but should not 404
        assert_ne!(resp.status(), 404, "status endpoint should exist");

        // GET /health → always works
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router().clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }
}
