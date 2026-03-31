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
    fn admission_control_basic() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        assert!(tracker.can_admit(2, 4096));
        assert!(!tracker.can_admit(10, 4096));
        assert!(!tracker.can_admit(2, 32768));
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
    fn release_resources() {
        let tracker = CapacityTracker::with_capacity(4, 8192);
        tracker.commit("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 2);
        tracker.release("vm-1", 2, 4096);
        assert_eq!(tracker.available_vcpus(), 4);
        assert_eq!(tracker.available_memory_mb(), 8192);
    }
}
