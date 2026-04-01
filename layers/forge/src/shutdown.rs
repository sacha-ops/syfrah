//! Graceful shutdown protocol.
//!
//! On SIGTERM:
//! 1. Stop accepting new requests
//! 2. Complete in-flight requests (30s grace period)
//! 3. Exit cleanly
//!
//! VMs continue running (they are separate processes).
//! On restart, the reconciler re-discovers all resources.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::signal;
use tokio::sync::watch;
use tracing::info;

/// Default grace period for in-flight request completion (30 seconds).
const DEFAULT_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// Shutdown controller for the Forge process.
pub struct ShutdownController {
    /// Sender to signal shutdown to the HTTP server.
    shutdown_tx: watch::Sender<bool>,
    /// Whether shutdown has been initiated.
    shutting_down: AtomicBool,
    /// Number of in-flight requests (for monitoring).
    in_flight: AtomicU64,
    /// Grace period for completing in-flight requests.
    grace_period: Duration,
}

impl ShutdownController {
    /// Create a new shutdown controller.
    pub fn new() -> (Self, watch::Receiver<bool>) {
        let (tx, rx) = watch::channel(false);
        let ctrl = Self {
            shutdown_tx: tx,
            shutting_down: AtomicBool::new(false),
            in_flight: AtomicU64::new(0),
            grace_period: DEFAULT_GRACE_PERIOD,
        };
        (ctrl, rx)
    }

    /// Check if shutdown has been initiated.
    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::SeqCst)
    }

    /// Increment in-flight request count.
    pub fn request_started(&self) {
        self.in_flight.fetch_add(1, Ordering::SeqCst);
    }

    /// Decrement in-flight request count.
    pub fn request_completed(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// Get current in-flight request count.
    pub fn in_flight_count(&self) -> u64 {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Initiate shutdown.
    pub fn initiate_shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::SeqCst) {
            return; // Already shutting down
        }
        info!("graceful shutdown initiated");
        let _ = self.shutdown_tx.send(true);
    }

    /// Wait for in-flight requests to complete or grace period to expire.
    pub async fn wait_for_drain(&self) {
        let start = Instant::now();
        loop {
            let count = self.in_flight_count();
            if count == 0 {
                info!("all in-flight requests completed");
                break;
            }
            if start.elapsed() > self.grace_period {
                info!(
                    remaining = count,
                    "grace period expired, proceeding with shutdown"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Default for ShutdownController {
    fn default() -> Self {
        Self::new().0
    }
}

/// Install signal handlers for graceful shutdown.
/// Returns when SIGTERM or SIGINT is received.
pub async fn wait_for_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("received SIGINT"); }
        _ = terminate => { info!("received SIGTERM"); }
    }
}

/// Run the graceful shutdown sequence.
pub async fn graceful_shutdown(controller: Arc<ShutdownController>) {
    wait_for_signal().await;
    controller.initiate_shutdown();
    controller.wait_for_drain().await;
    info!("forge shutdown complete — VMs continue running as separate processes");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_controller_initial_state() {
        let (ctrl, _rx) = ShutdownController::new();
        assert!(!ctrl.is_shutting_down());
        assert_eq!(ctrl.in_flight_count(), 0);
    }

    #[test]
    fn request_counting() {
        let (ctrl, _rx) = ShutdownController::new();
        ctrl.request_started();
        ctrl.request_started();
        assert_eq!(ctrl.in_flight_count(), 2);
        ctrl.request_completed();
        assert_eq!(ctrl.in_flight_count(), 1);
        ctrl.request_completed();
        assert_eq!(ctrl.in_flight_count(), 0);
    }

    #[test]
    fn initiate_shutdown_idempotent() {
        let (ctrl, _rx) = ShutdownController::new();
        ctrl.initiate_shutdown();
        assert!(ctrl.is_shutting_down());
        ctrl.initiate_shutdown(); // Should not panic
        assert!(ctrl.is_shutting_down());
    }

    #[tokio::test]
    async fn drain_completes_when_no_requests() {
        let (ctrl, _rx) = ShutdownController::new();
        ctrl.wait_for_drain().await;
        // Should return immediately
    }
}
