//! Signed request authentication middleware.
//!
//! Phase 1 (WireGuard trust domain):
//! - Check for Authorization header; validate if present
//! - Allow if absent (Phase 1 — WireGuard provides the trust boundary)
//! - Log caller identity when header is present
//!
//! Future: reject unsigned requests when control plane exists.

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use tracing::{info, warn};

/// Authentication mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// Phase 1: WireGuard trust domain. Allow unsigned requests.
    WireGuardTrust,
    /// Future: require signed requests.
    RequireSigned,
}

/// Authentication middleware for Forge HTTP API.
///
/// In WireGuardTrust mode:
/// - If Authorization header is present: validate and log caller identity
/// - If absent: allow (WireGuard provides the trust boundary)
///
/// In RequireSigned mode:
/// - Authorization header is required
/// - Reject unsigned requests with 401
pub async fn auth_middleware(request: Request, next: Next) -> Response {
    let auth_header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Phase 1: WireGuard trust domain — allow all, log identity if present.
    let mode = AuthMode::WireGuardTrust;

    match mode {
        AuthMode::WireGuardTrust => {
            if let Some(ref auth) = auth_header {
                // Validate the token format (Bearer <token>)
                if let Some(token) = auth.strip_prefix("Bearer ") {
                    let caller = extract_caller_identity(token);
                    info!(caller = %caller, "authenticated request");
                } else {
                    warn!(header = %auth, "malformed Authorization header (expected Bearer)");
                }
            }
            // Allow regardless — WireGuard mesh is the trust boundary.
            next.run(request).await
        }
        AuthMode::RequireSigned => {
            if auth_header.is_none() {
                return (StatusCode::UNAUTHORIZED, "missing Authorization header").into_response();
            }
            let auth = auth_header.unwrap();
            if let Some(token) = auth.strip_prefix("Bearer ") {
                if validate_token(token) {
                    let caller = extract_caller_identity(token);
                    info!(caller = %caller, "authenticated request");
                    next.run(request).await
                } else {
                    (StatusCode::UNAUTHORIZED, "invalid token").into_response()
                }
            } else {
                (StatusCode::UNAUTHORIZED, "malformed Authorization header").into_response()
            }
        }
    }
}

/// Extract caller identity from a token (placeholder).
/// In a real implementation, this would decode the JWT or signed payload.
fn extract_caller_identity(token: &str) -> String {
    // For now, just use the first 8 characters as an identity hint.
    if token.len() > 8 {
        format!("caller-{}", &token[..8])
    } else {
        format!("caller-{token}")
    }
}

/// Validate a bearer token (placeholder).
/// In a real implementation, this would verify the signature.
fn validate_token(_token: &str) -> bool {
    // Phase 1: all tokens are accepted (WireGuard trust).
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_caller_truncates() {
        let identity = extract_caller_identity("abcdefghijklmnop");
        assert_eq!(identity, "caller-abcdefgh");
    }

    #[test]
    fn extract_caller_short_token() {
        let identity = extract_caller_identity("abc");
        assert_eq!(identity, "caller-abc");
    }

    #[test]
    fn validate_token_always_true_in_phase1() {
        assert!(validate_token("any-token"));
        assert!(validate_token(""));
    }
}
