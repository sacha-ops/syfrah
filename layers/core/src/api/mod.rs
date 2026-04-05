//! API layer — auto-generates REST routes from ResourceDef.
//!
//! Layers export pure handlers. This module wraps them in HTTP:
//! - Routing: ResourceDef → axum routes
//! - Request parsing: JSON body + path params → OperationRequest
//! - Response rendering: OperationResponse → JSON HTTP response
//! - Error handling: SyfrahError → HTTP status + JSON error body
//! - Middleware: auth, logging, rate limiting
//!
//! # Usage
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
mod route_gen;
mod server;

pub use error_response::*;
pub use route_gen::*;
pub use server::*;
