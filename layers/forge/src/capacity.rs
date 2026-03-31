//! Capacity tracking and admission control.
//!
//! Tracks total, used, reserved, and available resources on this node.
//! Answers admission queries ("can this node fit a 4-vCPU VM?") and
//! manages reservations with expiry.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Reservation expiry duration (60 seconds).
const RESERVATION_EXPIRY: Duration = Duration::from_secs(60);

/// Node capacity snapshot.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NodeCapacity {
    /// Total vCPUs on this node.
    pub total_vcpus: u32,
    /// Total memory in MB on this node.
    pub total_memory_mb: u64,
    /// vCPUs currently allocated to VMs.
    pub used_vcpus: u32,
    /// Memory currently allocated to VMs in MB.
    pub used_memory_mb: u64,
}

/// A resource reservation with expiry.
#[derive(Debug, Clone)]
pub struct Reservation {
    pub vcpus: u32,
    pub memory_mb: u64,
    pub created_at: Instant,
}

/// Capacity tracker for admission control.
pub struct CapacityTracker {
    total_vcpus: u32,
    total_memory_mb: u64,
    used_vcpus: Mutex<u32>,
    used_memory_mb: Mutex<u64>,
    reservations: Mutex<HashMap<String, Reservation>>,
}

impl CapacityTracker {
    /// Create a new capacity tracker.
    ///
    /// Allocatable is computed as:
    /// - vCPUs: (total_cpus - 1) * 2
    /// - memory: total_memory - 1GB
    pub fn new() -> Self {
        let sys_cpus = num_cpus();
        let sys_mem = total_memory_mb();

        Self {
            total_vcpus: if sys_cpus > 1 { (sys_cpus - 1) * 2 } else { 1 },
            total_memory_mb: if sys_mem > 1024 {
                sys_mem - 1024
            } else {
                sys_mem
            },
            used_vcpus: Mutex::new(0),
            used_memory_mb: Mutex::new(0),
            reservations: Mutex::new(HashMap::new()),
        }
    }

    /// Create a tracker with explicit capacity (for testing).
    pub fn with_capacity(total_vcpus: u32, total_memory_mb: u64) -> Self {
        Self {
            total_vcpus,
            total_memory_mb,
            used_vcpus: Mutex::new(0),
            used_memory_mb: Mutex::new(0),
            reservations: Mutex::new(HashMap::new()),
        }
    }

    /// Get the allocatable vCPUs.
    pub fn allocatable_vcpus(&self) -> u32 {
        self.total_vcpus
    }

    /// Get the allocatable memory in MB.
    pub fn allocatable_memory_mb(&self) -> u64 {
        self.total_memory_mb
    }

    /// Get the available vCPUs (allocatable - used - reserved).
    pub fn available_vcpus(&self) -> u32 {
        let used = *self.used_vcpus.lock().unwrap();
        let reserved = self.reserved_vcpus();
        self.total_vcpus.saturating_sub(used + reserved)
    }

    /// Get the available memory in MB.
    pub fn available_memory_mb(&self) -> u64 {
        let used = *self.used_memory_mb.lock().unwrap();
        let reserved = self.reserved_memory_mb();
        self.total_memory_mb.saturating_sub(used + reserved)
    }

    fn reserved_vcpus(&self) -> u32 {
        let reservations = self.reservations.lock().unwrap();
        reservations
            .values()
            .filter(|r| r.created_at.elapsed() < RESERVATION_EXPIRY)
            .map(|r| r.vcpus)
            .sum()
    }

    fn reserved_memory_mb(&self) -> u64 {
        let reservations = self.reservations.lock().unwrap();
        reservations
            .values()
            .filter(|r| r.created_at.elapsed() < RESERVATION_EXPIRY)
            .map(|r| r.memory_mb)
            .sum()
    }

    /// Check if resources can be admitted.
    pub fn can_admit(&self, vcpus: u32, memory_mb: u64) -> bool {
        self.available_vcpus() >= vcpus && self.available_memory_mb() >= memory_mb
    }

    /// Reserve resources for an upcoming creation (60s expiry).
    pub fn reserve(&self, id: &str, vcpus: u32, memory_mb: u64) {
        let mut reservations = self.reservations.lock().unwrap();
        // Expire old reservations first
        reservations.retain(|_, r| r.created_at.elapsed() < RESERVATION_EXPIRY);
        reservations.insert(
            id.to_string(),
            Reservation {
                vcpus,
                memory_mb,
                created_at: Instant::now(),
            },
        );
    }

    /// Commit a reservation — move from reserved to used.
    pub fn commit(&self, id: &str, vcpus: u32, memory_mb: u64) {
        let mut reservations = self.reservations.lock().unwrap();
        reservations.remove(id);
        drop(reservations);

        *self.used_vcpus.lock().unwrap() += vcpus;
        *self.used_memory_mb.lock().unwrap() += memory_mb;
    }

    /// Release resources when a VM is deleted.
    pub fn release(&self, id: &str, vcpus: u32, memory_mb: u64) {
        let mut reservations = self.reservations.lock().unwrap();
        reservations.remove(id);
        drop(reservations);

        let mut used_v = self.used_vcpus.lock().unwrap();
        *used_v = used_v.saturating_sub(vcpus);
        drop(used_v);

        let mut used_m = self.used_memory_mb.lock().unwrap();
        *used_m = used_m.saturating_sub(memory_mb);
    }

    /// Update used resources from actual VM list.
    pub fn sync_used(&self, vcpus: u32, memory_mb: u64) {
        *self.used_vcpus.lock().unwrap() = vcpus;
        *self.used_memory_mb.lock().unwrap() = memory_mb;
    }

    /// Get a snapshot of current capacity.
    pub fn snapshot(&self) -> NodeCapacity {
        NodeCapacity {
            total_vcpus: self.total_vcpus,
            total_memory_mb: self.total_memory_mb,
            used_vcpus: *self.used_vcpus.lock().unwrap(),
            used_memory_mb: *self.used_memory_mb.lock().unwrap(),
        }
    }
}

impl Default for CapacityTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the number of CPUs from the system.
fn num_cpus() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(1)
}

/// Read total memory from /proc/meminfo.
fn total_memory_mb() -> u64 {
    let content = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return kb / 1024;
        }
    }
    4096 // fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admit_when_capacity_available() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        assert!(tracker.can_admit(2, 4096));
        assert!(tracker.can_admit(8, 16384)); // exact fit
    }

    #[test]
    fn reject_when_insufficient() {
        let tracker = CapacityTracker::with_capacity(4, 8192);
        // Too many vCPUs
        assert!(!tracker.can_admit(5, 4096));
        // Too much memory
        assert!(!tracker.can_admit(2, 16384));
        // Both exceed
        assert!(!tracker.can_admit(10, 32768));
    }

    #[test]
    fn reservation_expires() {
        let tracker = CapacityTracker::with_capacity(4, 8192);

        // Insert a reservation with a past timestamp (simulate expiry).
        {
            let mut reservations = tracker.reservations.lock().unwrap();
            reservations.insert(
                "expired-vm".to_string(),
                Reservation {
                    vcpus: 4,
                    memory_mb: 8192,
                    created_at: Instant::now() - Duration::from_secs(120),
                },
            );
        }

        // Expired reservation should not block admission.
        assert!(tracker.can_admit(4, 8192));
        assert_eq!(tracker.available_vcpus(), 4);
        assert_eq!(tracker.available_memory_mb(), 8192);
    }

    #[test]
    fn reservation_released_on_failure() {
        let tracker = CapacityTracker::with_capacity(4, 8192);

        // Reserve resources.
        tracker.reserve("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 2);
        assert_eq!(tracker.available_memory_mb(), 4096);

        // Simulate failure: release instead of commit.
        tracker.release("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 4);
        assert_eq!(tracker.available_memory_mb(), 8192);
    }

    #[test]
    fn overcommit_ratio_applied() {
        // On a system with e.g. 4 CPUs and 8GB, allocatable should be:
        // vCPUs: (4-1)*2 = 6, memory: 8192-1024 = 7168
        let tracker = CapacityTracker::with_capacity(6, 7168);
        assert_eq!(tracker.allocatable_vcpus(), 6);
        assert_eq!(tracker.allocatable_memory_mb(), 7168);

        // Can fit a 4-vCPU VM thanks to 2:1 overcommit
        assert!(tracker.can_admit(4, 4096));
        // But not 7 vCPUs
        assert!(!tracker.can_admit(7, 4096));
    }

    #[test]
    fn reserve_and_commit() {
        let tracker = CapacityTracker::with_capacity(4, 8192);
        tracker.reserve("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 2);
        assert_eq!(tracker.available_memory_mb(), 4096);

        tracker.commit("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 2);
        assert_eq!(tracker.available_memory_mb(), 4096);
    }

    #[test]
    fn multiple_reservations_stack() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        tracker.reserve("vm-1", 2, 4096);
        tracker.reserve("vm-2", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 4);
        assert_eq!(tracker.available_memory_mb(), 8192);

        // Third reservation should be rejected by admission
        assert!(!tracker.can_admit(5, 8192));
        assert!(tracker.can_admit(4, 8192));
    }

    #[test]
    fn snapshot_reflects_current_state() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        tracker.commit("vm-1", 2, 4096);

        let snap = tracker.snapshot();
        assert_eq!(snap.total_vcpus, 8);
        assert_eq!(snap.total_memory_mb, 16384);
        assert_eq!(snap.used_vcpus, 2);
        assert_eq!(snap.used_memory_mb, 4096);
    }

    #[test]
    fn sync_used_updates_counters() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        tracker.sync_used(4, 8192);
        assert_eq!(tracker.available_vcpus(), 4);
        assert_eq!(tracker.available_memory_mb(), 8192);
    }
}
