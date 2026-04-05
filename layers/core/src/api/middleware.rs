//! API middleware: rate limiting, request ID, versioning.

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

// ═══════════════════════════════════════════════════
// 5. Request ID
// ═══════════════════════════════════════════════════

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Middleware that injects `X-Request-Id` header into every response.
pub async fn request_id(req: Request, next: Next) -> Response {
    let id = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    let request_id = format!("req-{id:012x}");

    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        "x-request-id",
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    resp
}

// ═══════════════════════════════════════════════════
// 6. Version header
// ═══════════════════════════════════════════════════

/// Middleware that injects `X-Syfrah-Version` header.
pub async fn version_header(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    resp.headers_mut().insert(
        "x-syfrah-version",
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    resp
}

// ═══════════════════════════════════════════════════
// 2. Rate limiting (simple token bucket)
// ═══════════════════════════════════════════════════

/// Simple in-memory rate limiter.
#[derive(Clone)]
pub struct RateLimiter {
    max_requests: u64,
    window_secs: u64,
    state: Arc<RateLimitState>,
}

struct RateLimitState {
    count: AtomicU64,
    window_start: std::sync::Mutex<Instant>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    pub fn new(max_requests: u64, window_secs: u64) -> Self {
        Self {
            max_requests,
            window_secs,
            state: Arc::new(RateLimitState {
                count: AtomicU64::new(0),
                window_start: std::sync::Mutex::new(Instant::now()),
            }),
        }
    }

    /// Check if a request is allowed. Returns remaining requests.
    /// Check if a request is allowed. Returns remaining requests or false if rejected.
    #[allow(clippy::result_unit_err)]
    pub fn check(&self) -> Result<u64, ()> {
        let mut start = self.state.window_start.lock().unwrap();
        let elapsed = start.elapsed().as_secs();

        // Reset window if expired
        if elapsed >= self.window_secs {
            *start = Instant::now();
            self.state.count.store(1, Ordering::Relaxed);
            return Ok(self.max_requests - 1);
        }

        let count = self.state.count.fetch_add(1, Ordering::Relaxed) + 1;
        if count > self.max_requests {
            Err(())
        } else {
            Ok(self.max_requests - count)
        }
    }

    /// Reset the limiter (for testing).
    pub fn reset(&self) {
        self.state.count.store(0, Ordering::Relaxed);
        *self.state.window_start.lock().unwrap() = Instant::now();
    }
}

// ═══════════════════════════════════════════════════
// 3. Pagination query params
// ═══════════════════════════════════════════════════

/// Standard pagination parameters extracted from query string.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Pagination {
    /// Number of items to return (default 50, max 200).
    #[serde(default = "default_limit")]
    pub limit: u64,
    /// Offset for pagination (default 0).
    #[serde(default)]
    pub offset: u64,
    /// Sort field.
    #[serde(default)]
    pub sort: Option<String>,
    /// Sort order: "asc" or "desc".
    #[serde(default = "default_order")]
    pub order: String,
}

fn default_limit() -> u64 {
    50
}
fn default_order() -> String {
    "asc".into()
}

impl Pagination {
    /// Clamp limit to max 200.
    pub fn clamped_limit(&self) -> u64 {
        self.limit.min(200)
    }
}

/// Paginated list response.
#[derive(Debug, serde::Serialize)]
pub struct PaginatedResponse<T: serde::Serialize> {
    pub items: Vec<T>,
    pub count: usize,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
    pub has_more: bool,
}

impl<T: serde::Serialize> PaginatedResponse<T> {
    pub fn new(items: Vec<T>, total: u64, pagination: &Pagination) -> Self {
        let count = items.len();
        let has_more = pagination.offset + (count as u64) < total;
        Self {
            items,
            count,
            total,
            offset: pagination.offset,
            limit: pagination.clamped_limit(),
            has_more,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Rate limiter ──

    #[test]
    fn rate_limiter_allows_within_window() {
        let rl = RateLimiter::new(5, 60);
        for _ in 0..5 {
            assert!(rl.check().is_ok());
        }
    }

    #[test]
    fn rate_limiter_rejects_over_limit() {
        let rl = RateLimiter::new(3, 60);
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_err());
    }

    #[test]
    fn rate_limiter_reset() {
        let rl = RateLimiter::new(2, 60);
        assert!(rl.check().is_ok());
        assert!(rl.check().is_ok());
        assert!(rl.check().is_err());
        rl.reset();
        assert!(rl.check().is_ok());
    }

    #[test]
    fn rate_limiter_returns_remaining() {
        let rl = RateLimiter::new(5, 60);
        assert_eq!(rl.check().unwrap(), 4);
        assert_eq!(rl.check().unwrap(), 3);
    }

    // ── Pagination ──

    #[test]
    fn pagination_defaults() {
        let p: Pagination = serde_json::from_str("{}").unwrap();
        assert_eq!(p.limit, 50);
        assert_eq!(p.offset, 0);
        assert_eq!(p.order, "asc");
    }

    #[test]
    fn pagination_clamped() {
        let p = Pagination {
            limit: 999,
            offset: 0,
            sort: None,
            order: "asc".into(),
        };
        assert_eq!(p.clamped_limit(), 200);
    }

    #[test]
    fn paginated_response_has_more() {
        let p = Pagination {
            limit: 2,
            offset: 0,
            sort: None,
            order: "asc".into(),
        };
        let resp = PaginatedResponse::new(vec!["a", "b"], 5, &p);
        assert!(resp.has_more);
        assert_eq!(resp.total, 5);
        assert_eq!(resp.count, 2);
    }

    #[test]
    fn paginated_response_no_more() {
        let p = Pagination {
            limit: 10,
            offset: 0,
            sort: None,
            order: "asc".into(),
        };
        let resp = PaginatedResponse::new(vec!["a", "b"], 2, &p);
        assert!(!resp.has_more);
    }

    #[test]
    fn paginated_response_serializes() {
        let p = Pagination {
            limit: 50,
            offset: 0,
            sort: None,
            order: "asc".into(),
        };
        let resp = PaginatedResponse::new(vec!["x"], 100, &p);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"has_more\":true"));
        assert!(json.contains("\"total\":100"));
    }

    // ── Request counter ──

    #[test]
    fn request_counter_increments() {
        let a = REQUEST_COUNTER.load(Ordering::Relaxed);
        REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let b = REQUEST_COUNTER.load(Ordering::Relaxed);
        assert_eq!(b, a + 1);
    }
}
