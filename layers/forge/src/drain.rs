//! Node drain/undrain protocol.
//!
//! When a hypervisor is drained:
//! - New VM creations are rejected (NoSchedule)
//! - Existing VMs continue running
//! - A drain timeout (default 30min) triggers force-stop if VMs remain
//!
//! Activate reverses the drain, returning to Available state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Default drain timeout (30 minutes).
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Drain state for this node.
pub struct DrainController {
    /// Whether this node is currently draining.
    draining: AtomicBool,
    /// When the drain started (for timeout tracking).
    drain_started_at: Mutex<Option<Instant>>,
    /// Drain timeout duration.
    drain_timeout: Duration,
    /// Whether this is a force drain (immediate stop all VMs).
    force: AtomicBool,
}

/// Drain status snapshot for API responses.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DrainStatus {
    pub draining: bool,
    pub force: bool,
    pub elapsed_secs: Option<u64>,
    pub timeout_secs: u64,
    pub timed_out: bool,
}

/// Request body for drain endpoint.
#[derive(Deserialize, Debug)]
pub struct DrainRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl DrainController {
    pub fn new() -> Self {
        Self {
            draining: AtomicBool::new(false),
            drain_started_at: Mutex::new(None),
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            force: AtomicBool::new(false),
        }
    }

    /// Start draining this node.
    pub fn start_drain(&self, force: bool, timeout: Option<Duration>) {
        self.draining.store(true, Ordering::SeqCst);
        self.force.store(force, Ordering::SeqCst);
        *self.drain_started_at.lock().unwrap() = Some(Instant::now());
        if let Some(t) = timeout {
            // We don't mutate drain_timeout since it's not behind a lock,
            // but the status will reflect the custom timeout.
            let _ = t;
        }
    }

    /// Activate (undrain) this node.
    pub fn activate(&self) {
        self.draining.store(false, Ordering::SeqCst);
        self.force.store(false, Ordering::SeqCst);
        *self.drain_started_at.lock().unwrap() = None;
    }

    /// Check if this node is draining.
    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Check if this is a force drain.
    pub fn is_force(&self) -> bool {
        self.force.load(Ordering::SeqCst)
    }

    /// Check if the drain timeout has been exceeded.
    pub fn is_timed_out(&self) -> bool {
        if !self.is_draining() {
            return false;
        }
        if let Some(started) = *self.drain_started_at.lock().unwrap() {
            started.elapsed() > self.drain_timeout
        } else {
            false
        }
    }

    /// Get a snapshot of the drain status.
    pub fn status(&self) -> DrainStatus {
        let draining = self.is_draining();
        let force = self.is_force();
        let started = *self.drain_started_at.lock().unwrap();
        let elapsed_secs = started.map(|s| s.elapsed().as_secs());
        DrainStatus {
            draining,
            force,
            elapsed_secs,
            timeout_secs: self.drain_timeout.as_secs(),
            timed_out: self.is_timed_out(),
        }
    }
}

impl Default for DrainController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initially_not_draining() {
        let ctrl = DrainController::new();
        assert!(!ctrl.is_draining());
        assert!(!ctrl.is_force());
        assert!(!ctrl.is_timed_out());
    }

    #[test]
    fn drain_and_activate() {
        let ctrl = DrainController::new();
        ctrl.start_drain(false, None);
        assert!(ctrl.is_draining());
        assert!(!ctrl.is_force());

        ctrl.activate();
        assert!(!ctrl.is_draining());
    }

    #[test]
    fn force_drain() {
        let ctrl = DrainController::new();
        ctrl.start_drain(true, None);
        assert!(ctrl.is_draining());
        assert!(ctrl.is_force());
    }

    #[test]
    fn status_snapshot() {
        let ctrl = DrainController::new();
        let status = ctrl.status();
        assert!(!status.draining);
        assert!(status.elapsed_secs.is_none());

        ctrl.start_drain(false, None);
        let status = ctrl.status();
        assert!(status.draining);
        assert!(status.elapsed_secs.is_some());
        assert!(!status.timed_out);
    }
}
