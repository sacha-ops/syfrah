//! API layer — auto-generates REST routes from ResourceDef.
//!
//! Layers export pure handlers. This module wraps them in HTTP:
//! - Routing: ResourceDef → axum routes (with scoped paths)
//! - Request parsing: JSON body + path params → OperationRequest
//! - Response rendering: OperationResponse → JSON HTTP response
//! - Error handling: SyfrahError → HTTP status + JSON error body
//! - Middleware: request ID, version header, rate limiting, tracing
//! - Pagination: standard query params on list endpoints
//! - OpenAPI: auto-generated spec from ResourceDef
//! - Graceful shutdown: SIGTERM/SIGINT handling
//!
//! ```no_run
//! use syfrah_core::api::{ApiServer, ApiConfig};
//!
//! # async fn example() {
//! let config = ApiConfig::default();
//! let server = ApiServer::new(config, vec![], vec![]);
//! // server.run_admin().await;
//! # }
//! ```

mod error_response;
pub mod middleware;
mod route_gen;
mod server;

pub use error_response::*;
pub use route_gen::*;
pub use server::*;
