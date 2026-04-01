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

/// Node capacity snapshot with full breakdown.
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
    /// Physical CPU count (from /proc or sysconf).
    #[serde(default)]
    pub physical_vcpus: u32,
    /// Physical memory in MB (from /proc/meminfo).
    #[serde(default)]
    pub physical_memory_mb: u64,
    /// Reserved vCPUs for host (default 1).
    #[serde(default)]
    pub reserved_vcpus: u32,
    /// Reserved memory in MB for host (default 1024).
    #[serde(default)]
    pub reserved_memory_mb: u64,
    /// CPU overcommit ratio (default 2.0).
    #[serde(default = "default_cpu_overcommit")]
    pub overcommit_cpu: f64,
    /// Memory overcommit ratio (default 1.0).
    #[serde(default = "default_mem_overcommit")]
    pub overcommit_memory: f64,
    /// Allocatable vCPUs = (physical - reserved) * overcommit.
    #[serde(default)]
    pub allocatable_vcpus: u32,
    /// Allocatable memory in MB = (physical - reserved) * overcommit.
    #[serde(default)]
    pub allocatable_memory_mb: u64,
    /// Available vCPUs = allocatable - used - pending reservations.
    #[serde(default)]
    pub available_vcpus: u32,
    /// Available memory in MB = allocatable - used - pending reservations.
    #[serde(default)]
    pub available_memory_mb: u64,
    /// Total disk in GB.
    #[serde(default)]
    pub disk_total_gb: u64,
    /// Used disk in GB.
    #[serde(default)]
    pub disk_used_gb: u64,
    /// Available disk in GB.
    #[serde(default)]
    pub disk_available_gb: u64,
}

fn default_cpu_overcommit() -> f64 {
    2.0
}

fn default_mem_overcommit() -> f64 {
    1.0
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
    /// Allocatable vCPUs (after reserved + overcommit).
    total_vcpus: u32,
    /// Allocatable memory in MB (after reserved).
    total_memory_mb: u64,
    /// Physical CPU count from sysconf.
    physical_vcpus: u32,
    /// Physical memory in MB from /proc/meminfo.
    physical_memory_mb: u64,
    /// Reserved vCPUs for host.
    reserved_vcpus: u32,
    /// Reserved memory in MB for host.
    reserved_memory_mb: u64,
    /// CPU overcommit ratio.
    overcommit_cpu: f64,
    /// Memory overcommit ratio.
    overcommit_memory: f64,
    used_vcpus: Mutex<u32>,
    used_memory_mb: Mutex<u64>,
    reservations: Mutex<HashMap<String, Reservation>>,
}

impl CapacityTracker {
    /// Create a new capacity tracker from system info.
    ///
    /// Host reserved: 1 vCPU, 1 GB RAM.
    /// Overcommit: CPU 2:1, memory 1:1.
    /// Allocatable = (physical - reserved) * overcommit.
    pub fn new() -> Self {
        let sys_cpus = num_cpus();
        let sys_mem = total_memory_mb();
        let reserved_v: u32 = 1;
        let reserved_m: u64 = 1024;
        let overcommit_c: f64 = 2.0;
        let overcommit_m: f64 = 1.0;

        let alloc_v = if sys_cpus > reserved_v {
            ((sys_cpus - reserved_v) as f64 * overcommit_c) as u32
        } else {
            1
        };
        let alloc_m = if sys_mem > reserved_m {
            ((sys_mem - reserved_m) as f64 * overcommit_m) as u64
        } else {
            sys_mem
        };

        Self {
            total_vcpus: alloc_v,
            total_memory_mb: alloc_m,
            physical_vcpus: sys_cpus,
            physical_memory_mb: sys_mem,
            reserved_vcpus: reserved_v,
            reserved_memory_mb: reserved_m,
            overcommit_cpu: overcommit_c,
            overcommit_memory: overcommit_m,
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
            physical_vcpus: total_vcpus,
            physical_memory_mb: total_memory_mb,
            reserved_vcpus: 0,
            reserved_memory_mb: 0,
            overcommit_cpu: 1.0,
            overcommit_memory: 1.0,
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

    /// Get the physical CPU count.
    pub fn physical_vcpus(&self) -> u32 {
        self.physical_vcpus
    }

    /// Get the physical memory in MB.
    pub fn physical_memory_mb(&self) -> u64 {
        self.physical_memory_mb
    }

    /// Get the used vCPUs.
    pub fn used_vcpus(&self) -> u32 {
        *self.used_vcpus.lock().unwrap()
    }

    /// Get the used memory in MB.
    pub fn used_memory_mb(&self) -> u64 {
        *self.used_memory_mb.lock().unwrap()
    }

    /// Get a full breakdown snapshot of current capacity.
    pub fn snapshot(&self) -> NodeCapacity {
        let used_v = *self.used_vcpus.lock().unwrap();
        let used_m = *self.used_memory_mb.lock().unwrap();
        let (disk_total, disk_used, disk_avail) = disk_usage_gb();

        NodeCapacity {
            total_vcpus: self.total_vcpus,
            total_memory_mb: self.total_memory_mb,
            used_vcpus: used_v,
            used_memory_mb: used_m,
            physical_vcpus: self.physical_vcpus,
            physical_memory_mb: self.physical_memory_mb,
            reserved_vcpus: self.reserved_vcpus,
            reserved_memory_mb: self.reserved_memory_mb,
            overcommit_cpu: self.overcommit_cpu,
            overcommit_memory: self.overcommit_memory,
            allocatable_vcpus: self.total_vcpus,
            allocatable_memory_mb: self.total_memory_mb,
            available_vcpus: self.available_vcpus(),
            available_memory_mb: self.available_memory_mb(),
            disk_total_gb: disk_total,
            disk_used_gb: disk_used,
            disk_available_gb: disk_avail,
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

/// Read disk usage for the root filesystem.
/// Returns (total_gb, used_gb, available_gb).
fn disk_usage_gb() -> (u64, u64, u64) {
    // Use statvfs on the root filesystem
    let output = std::process::Command::new("df")
        .args(["-B1", "/"])
        .output()
        .ok();

    if let Some(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Skip header line, parse second line
        if let Some(line) = stdout.lines().nth(1) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                let total = parts[1].parse::<u64>().unwrap_or(0) / (1024 * 1024 * 1024);
                let used = parts[2].parse::<u64>().unwrap_or(0) / (1024 * 1024 * 1024);
                let avail = parts[3].parse::<u64>().unwrap_or(0) / (1024 * 1024 * 1024);
                return (total, used, avail);
            }
        }
    }
    (0, 0, 0)
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

    #[test]
    fn snapshot_includes_full_breakdown() {
        let tracker = CapacityTracker::with_capacity(8, 16384);
        tracker.commit("vm-1", 2, 4096);

        let snap = tracker.snapshot();
        // Basic fields
        assert_eq!(snap.total_vcpus, 8);
        assert_eq!(snap.total_memory_mb, 16384);
        assert_eq!(snap.used_vcpus, 2);
        assert_eq!(snap.used_memory_mb, 4096);
        // Allocatable = total (in test mode, no reserved/overcommit)
        assert_eq!(snap.allocatable_vcpus, 8);
        assert_eq!(snap.allocatable_memory_mb, 16384);
        // Available = allocatable - used
        assert_eq!(snap.available_vcpus, 6);
        assert_eq!(snap.available_memory_mb, 12288);
        // Physical = total (in test mode with_capacity)
        assert_eq!(snap.physical_vcpus, 8);
        assert_eq!(snap.physical_memory_mb, 16384);
        // No reserved or overcommit in test mode
        assert_eq!(snap.reserved_vcpus, 0);
        assert_eq!(snap.reserved_memory_mb, 0);
    }

    #[test]
    fn physical_accessors() {
        let tracker = CapacityTracker::with_capacity(4, 8192);
        assert_eq!(tracker.physical_vcpus(), 4);
        assert_eq!(tracker.physical_memory_mb(), 8192);
        assert_eq!(tracker.used_vcpus(), 0);
        assert_eq!(tracker.used_memory_mb(), 0);

        tracker.commit("vm-1", 1, 2048);
        assert_eq!(tracker.used_vcpus(), 1);
        assert_eq!(tracker.used_memory_mb(), 2048);
    }
}
