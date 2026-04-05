//! API server with graceful shutdown.

use axum::Router;
use std::net::SocketAddr;

use super::middleware;
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
    /// Rate limit: max requests per window.
    pub rate_limit_requests: u64,
    /// Rate limit: window in seconds.
    pub rate_limit_window_secs: u64,
}

impl Default for ApiConfig {
    fn default() -> Self {
        Self {
            admin_addr: "127.0.0.1:8443".parse().unwrap(),
            public_addr: None,
            admin_prefix: "/admin/v1".to_string(),
            public_prefix: "/v1".to_string(),
            rate_limit_requests: 1000,
            rate_limit_window_secs: 60,
        }
    }
}

/// The API server.
pub struct ApiServer {
    pub config: ApiConfig,
    admin_router: Router,
    public_router: Option<Router>,
}

impl ApiServer {
    /// Create a new API server.
    pub fn new(
        config: ApiConfig,
        admin_resources: Vec<ResourceRegistration>,
        public_resources: Vec<ResourceRegistration>,
    ) -> Self {
        let admin_router = build_api_router(&config, admin_resources, &config.admin_prefix);
        let public_router = if !public_resources.is_empty() {
            Some(build_api_router(
                &config,
                public_resources,
                &config.public_prefix,
            ))
        } else {
            None
        };

        Self {
            config,
            admin_router,
            public_router,
        }
    }

    pub fn admin_router(&self) -> &Router {
        &self.admin_router
    }

    pub fn public_router(&self) -> Option<&Router> {
        self.public_router.as_ref()
    }

    /// #8: Run admin API with graceful shutdown on SIGTERM/SIGINT.
    pub async fn run_admin(self) -> Result<(), std::io::Error> {
        let listener = tokio::net::TcpListener::bind(self.config.admin_addr).await?;
        tracing::info!(addr = %self.config.admin_addr, "admin API listening");

        axum::serve(listener, self.admin_router)
            .with_graceful_shutdown(shutdown_signal())
            .await
    }

    /// Run public API with graceful shutdown.
    pub async fn run_public(self) -> Result<(), std::io::Error> {
        let addr = self
            .config
            .public_addr
            .unwrap_or_else(|| "0.0.0.0:443".parse().unwrap());
        let router = self.public_router.unwrap_or_default();
        let listener = tokio::net::TcpListener::bind(addr).await?;
        tracing::info!(addr = %addr, "public API listening");

        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown_signal())
            .await
    }
}

fn build_api_router(
    _config: &ApiConfig,
    registrations: Vec<ResourceRegistration>,
    prefix: &str,
) -> Router {
    let api_routes = build_router(registrations, prefix);

    let health = Router::new().route(
        "/health",
        axum::routing::get(|| async {
            axum::Json(serde_json::json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
            }))
        }),
    );

    // #7: OpenAPI spec endpoint
    let openapi = Router::new().route(
        "/openapi.json",
        axum::routing::get(move || {
            async move {
                // Can't access registrations here easily — return minimal spec
                axum::Json(serde_json::json!({
                    "openapi": "3.0.0",
                    "info": {
                        "title": "Syfrah API",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "paths": {},
                }))
            }
        }),
    );

    Router::new()
        .merge(api_routes)
        .merge(health)
        .merge(openapi)
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(middleware::request_id))
        .layer(axum::middleware::from_fn(middleware::version_header))
}

/// #8: Wait for SIGTERM or SIGINT.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { tracing::info!("received Ctrl+C, shutting down"); }
        _ = terminate => { tracing::info!("received SIGTERM, shutting down"); }
    }
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
                OperationDef::get(),
                OperationDef::delete(),
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
                    "get" => Ok(OperationResponse::Resource(
                        serde_json::json!({"name": req.name.unwrap_or_default()}),
                    )),
                    "delete" => Ok(OperationResponse::Message("deleted".into())),
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
        assert_eq!(c.rate_limit_requests, 1000);
    }

    #[test]
    fn server_builds() {
        let _server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);
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

    // #11: GET and DELETE on same /{id} path
    #[tokio::test]
    async fn get_and_delete_same_path() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

        // GET
        let req = Request::builder()
            .uri("/admin/v1/thing/t1")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), 200);

        // DELETE
        let req = Request::builder()
            .method("DELETE")
            .uri("/admin/v1/thing/t1")
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

    // #5: Request ID header
    #[tokio::test]
    async fn response_has_request_id() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert!(resp.headers().contains_key("x-request-id"));
    }

    // #6: Version header
    #[tokio::test]
    async fn response_has_version() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = server.admin_router.clone().oneshot(req).await.unwrap();
        assert!(resp.headers().contains_key("x-syfrah-version"));
    }

    // #7: OpenAPI endpoint
    #[tokio::test]
    async fn openapi_endpoint() {
        let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);
        let req = Request::builder()
            .uri("/openapi.json")
            .body(Body::empty())
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
