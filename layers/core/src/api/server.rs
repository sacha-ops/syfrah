//! API server configuration and startup.

use axum::Router;
use std::net::SocketAddr;

use super::route_gen::build_router;
use crate::resource::ResourceRegistration;

/// API server configuration.
#[derive(Debug, Clone)]
pub struct ApiConfig {
    /// Admin API bind address.
    pub admin_addr: SocketAddr,
    /// Public API bind address (None = disabled).
    pub public_addr: Option<SocketAddr>,
    /// API prefix for admin routes.
    pub admin_prefix: String,
    /// API prefix for public routes.
    pub public_prefix: String,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            admin_addr: "127.0.0.1:8443".parse().unwrap(),
            public_addr: None,
            admin_prefix: "/admin/v1".to_string(),
            public_prefix: "/v1".to_string(),
        }
    }
}

/// The API server — holds config and builds the router.
pub struct ApiServer {
    pub config: ApiConfig,
    admin_router: Router,
    public_router: Option<Router>,
}

impl ApiServer {
    /// Create a new API server from config and resource registrations.
    pub fn new(
        config: ApiConfig,
        admin_resources: Vec<ResourceRegistration>,
        public_resources: Vec<ResourceRegistration>,
    ) -> Self {
        let admin_router = build_admin_router(&config, admin_resources);
        let public_router = if !public_resources.is_empty() {
            Some(build_public_router(&config, public_resources))
        } else {
            None
        };

        Self {
            config,
            admin_router,
            public_router,
        }
    }

    /// Get the admin router (for testing or embedding).
    pub fn admin_router(&self) -> &Router {
        &self.admin_router
    }

    /// Get the public router (for testing or embedding).
    pub fn public_router(&self) -> Option<&Router> {
        self.public_router.as_ref()
    }

    /// Run the admin API server. Blocks until shutdown.
    pub async fn run_admin(self) -> Result<(), std::io::Error> {
        let listener = tokio::net::TcpListener::bind(self.config.admin_addr).await?;
        tracing::info!(addr = %self.config.admin_addr, "admin API listening");
        axum::serve(listener, self.admin_router).await
    }
}

fn build_admin_router(config: &ApiConfig, registrations: Vec<ResourceRegistration>) -> Router {
    let api_routes = build_router(registrations, &config.admin_prefix);

    // Health check
    let health = Router::new().route(
        "/health",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }),
    );

    // Routes info endpoint
    Router::new()
        .merge(api_routes)
        .merge(health)
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

fn build_public_router(config: &ApiConfig, registrations: Vec<ResourceRegistration>) -> Router {
    let api_routes = build_router(registrations, &config.public_prefix);

    Router::new()
        .merge(api_routes)
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::*;
    use axum::body::Body;
    use http::Request;
    use tower::ServiceExt;

    fn test_resource() -> ResourceRegistration {
        let def = ResourceDef {
            identity: ResourceIdentity {
                kind: "thing",
                cli_name: "thing",
                plural: "things",
                description: "Test thing",
                aliases: &[],
            },
            scope: ScopeDef::global(),
            schema: ResourceSchema::new(),
            operations: vec![
                OperationDef::list(),
                OperationDef::action("ping", "Ping the thing"),
            ],
            presentation: PresentationDef::none(),
        };

        let handler: HandlerFn = Box::new(|req| {
            Box::pin(async move {
                match req.operation.as_str() {
                    "list" => Ok(OperationResponse::ResourceList(vec![
                        serde_json::json!({"name": "t1"}),
                    ])),
                    "ping" => Ok(OperationResponse::Message("pong".into())),
                    _ => Ok(OperationResponse::None),
                }
            })
        });

        ResourceRegistration { def, handler }
    }

    #[test]
    fn default_config() {
        let c = ApiConfig::default();
        assert_eq!(c.admin_addr.port(), 8443);
        assert_eq!(c.admin_prefix, "/admin/v1");
        assert!(c.public_addr.is_none());
    }

    #[test]
    fn server_builds() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);
        assert!(server.public_router().is_none());
    }

    #[tokio::test]
    async fn health_endpoint() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();

        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn list_endpoint() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

        let req = Request::builder()
            .uri("/admin/v1/thing")
            .body(Body::empty())
            .unwrap();

        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn action_endpoint() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

        let req = Request::builder()
            .method("POST")
            .uri("/admin/v1/thing/ping")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();

        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);
    }

    #[tokio::test]
    async fn not_found_route() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

        let req = Request::builder()
            .uri("/admin/v1/nonexistent")
            .body(Body::empty())
            .unwrap();

        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 404);
    }
}
