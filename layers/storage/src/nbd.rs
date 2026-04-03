//! NBD (Network Block Device) allocator for `/dev/nbdN` device management.
//!
//! This module tracks which `/dev/nbdN` slots are in use so that ZeroFS
//! instances each get a unique block device. The allocator does **not** manage
//! the kernel NBD connection itself — ZeroFS takes a device path and handles
//! the `nbd-client`/ioctl lifecycle.
//!
//! # Edge cases handled
//!
//! - Kernel module not loaded (`modprobe nbd` required)
//! - All device slots exhausted
//! - Device file missing from `/dev` even though the slot number is in range
//! - Concurrent allocation via interior `Mutex`

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Default maximum number of NBD devices if the kernel module parameter cannot
/// be read.
const DEFAULT_MAX_DEVICES: usize = 16;

/// Sysfs path where the `nbd` module's `nbds_max` parameter is exposed.
const NBDS_MAX_PATH: &str = "/sys/module/nbd/parameters/nbds_max";

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors returned by NBD allocator operations.
#[derive(Debug, thiserror::Error)]
pub enum NbdError {
    /// The `nbd` kernel module is not loaded and `modprobe` failed.
    #[error("failed to load nbd kernel module: {reason}")]
    ModuleLoadFailed { reason: String },

    /// All `/dev/nbdN` device slots are currently in use.
    #[error("all {max} NBD device slots are in use")]
    AllDevicesBusy { max: usize },

    /// The `/dev/nbdN` device file does not exist on disk.
    #[error("device file does not exist: {path}")]
    DeviceFileMissing { path: String },

    /// Attempted to release a device that was not allocated.
    #[error("device /dev/nbd{n} is not currently allocated")]
    NotAllocated { n: usize },

    /// An I/O error occurred while probing the system.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// NbdDevice
// ---------------------------------------------------------------------------

/// A handle representing an allocated `/dev/nbdN` device.
///
/// This is a plain data struct — dropping it does **not** automatically release
/// the slot. Call [`NbdAllocator::release`] explicitly when the device is no
/// longer needed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NbdDevice {
    /// The device number (the `N` in `/dev/nbdN`).
    pub number: usize,
}

impl NbdDevice {
    /// The full device path, e.g. `/dev/nbd0`.
    ///
    /// Derived from `number` so there is only one source of truth.
    pub fn path(&self) -> PathBuf {
        PathBuf::from(format!("/dev/nbd{}", self.number))
    }
}

impl fmt::Display for NbdDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/dev/nbd{}", self.number)
    }
}

// ---------------------------------------------------------------------------
// System interaction trait (for testing)
// ---------------------------------------------------------------------------

/// Trait abstracting system-level operations so unit tests can run without real
/// kernel devices.
pub trait NbdSystem: Send + Sync {
    /// Run `modprobe nbd` (or equivalent). Returns `Ok(())` on success.
    fn load_module(&self) -> Result<(), NbdError>;

    /// Check whether the `nbd` kernel module is currently loaded.
    fn is_module_loaded(&self) -> bool;

    /// Read the `nbds_max` kernel parameter. Returns `None` if the parameter
    /// file cannot be read.
    fn read_max_devices(&self) -> Option<usize>;

    /// Check whether `/dev/nbdN` exists on disk.
    fn device_file_exists(&self, n: usize) -> bool;
}

/// Real system implementation that shells out to `modprobe` and reads sysfs.
pub struct RealNbdSystem;

impl NbdSystem for RealNbdSystem {
    fn load_module(&self) -> Result<(), NbdError> {
        let output = std::process::Command::new("modprobe")
            .arg("nbd")
            .output()
            .map_err(|e| NbdError::ModuleLoadFailed {
                reason: format!("failed to execute modprobe: {e}"),
            })?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(NbdError::ModuleLoadFailed {
                reason: format!(
                    "modprobe nbd exited with {}: {}",
                    output.status,
                    stderr.trim()
                ),
            })
        }
    }

    fn is_module_loaded(&self) -> bool {
        Path::new("/sys/module/nbd").exists()
    }

    fn read_max_devices(&self) -> Option<usize> {
        std::fs::read_to_string(NBDS_MAX_PATH)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }

    fn device_file_exists(&self, n: usize) -> bool {
        Path::new(&format!("/dev/nbd{n}")).exists()
    }
}

// ---------------------------------------------------------------------------
// NbdAllocator
// ---------------------------------------------------------------------------

/// Tracks which `/dev/nbdN` device slots are in use.
///
/// Thread-safe via an interior `Mutex`. The allocator does not manage kernel
/// NBD connections — it only reserves and releases device numbers.
pub struct NbdAllocator<S: NbdSystem = RealNbdSystem> {
    system: S,
    state: Mutex<AllocatorState>,
}

#[derive(Debug)]
struct AllocatorState {
    /// Set of device numbers currently allocated.
    in_use: HashSet<usize>,
    /// Cached maximum device count.
    max_devices: usize,
}

impl Default for NbdAllocator<RealNbdSystem> {
    fn default() -> Self {
        Self::new()
    }
}

impl NbdAllocator<RealNbdSystem> {
    /// Create a new allocator backed by real system calls.
    pub fn new() -> Self {
        Self::with_system(RealNbdSystem)
    }
}

impl<S: NbdSystem> NbdAllocator<S> {
    /// Create a new allocator with a custom system backend (useful for tests).
    pub fn with_system(system: S) -> Self {
        let max_devices = system.read_max_devices().unwrap_or(DEFAULT_MAX_DEVICES);
        Self {
            system,
            state: Mutex::new(AllocatorState {
                in_use: HashSet::new(),
                max_devices,
            }),
        }
    }

    /// Ensure the `nbd` kernel module is loaded. Calls `modprobe nbd` if the
    /// module is not already present.
    pub fn load_module(&self) -> Result<(), NbdError> {
        if self.system.is_module_loaded() {
            return Ok(());
        }
        self.system.load_module()
    }

    /// Return the maximum number of NBD device slots available.
    pub fn max_devices(&self) -> usize {
        let state = self.state.lock().expect("allocator lock poisoned");
        state.max_devices
    }

    /// Check whether `/dev/nbdN` exists and is not currently allocated.
    pub fn is_available(&self, n: usize) -> bool {
        let state = self.state.lock().expect("allocator lock poisoned");
        if n >= state.max_devices {
            return false;
        }
        if state.in_use.contains(&n) {
            return false;
        }
        self.system.device_file_exists(n)
    }

    /// Allocate the next free `/dev/nbdN` device.
    ///
    /// Scans from `nbd0` upward, skipping slots that are in use or whose
    /// device file is missing. Returns the allocated device or an error if
    /// every slot is occupied.
    pub fn allocate(&self) -> Result<NbdDevice, NbdError> {
        let mut state = self.state.lock().expect("allocator lock poisoned");
        let max = state.max_devices;

        for n in 0..max {
            if state.in_use.contains(&n) {
                continue;
            }
            if !self.system.device_file_exists(n) {
                continue;
            }
            state.in_use.insert(n);
            return Ok(NbdDevice { number: n });
        }

        Err(NbdError::AllDevicesBusy { max })
    }

    /// Release a previously allocated device, making its slot available again.
    pub fn release(&self, device: &NbdDevice) -> Result<(), NbdError> {
        let mut state = self.state.lock().expect("allocator lock poisoned");
        if !state.in_use.remove(&device.number) {
            return Err(NbdError::NotAllocated { n: device.number });
        }
        Ok(())
    }

    /// Return the number of currently allocated device slots.
    pub fn allocated_count(&self) -> usize {
        let state = self.state.lock().expect("allocator lock poisoned");
        state.in_use.len()
    }

    /// Return a sorted list of currently allocated device numbers.
    pub fn allocated_devices(&self) -> Vec<usize> {
        let state = self.state.lock().expect("allocator lock poisoned");
        let mut v: Vec<usize> = state.in_use.iter().copied().collect();
        v.sort_unstable();
        v
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Fake system backend for unit tests.
    struct FakeSystem {
        module_loaded: AtomicBool,
        max_devices: usize,
        /// Which device files "exist" (by number).
        existing_devices: HashSet<usize>,
    }

    impl FakeSystem {
        fn new(max: usize, existing: &[usize]) -> Self {
            Self {
                module_loaded: AtomicBool::new(true),
                max_devices: max,
                existing_devices: existing.iter().copied().collect(),
            }
        }

        fn with_module_unloaded(max: usize, existing: &[usize]) -> Self {
            let sys = Self::new(max, existing);
            sys.module_loaded.store(false, Ordering::SeqCst);
            sys
        }
    }

    impl NbdSystem for FakeSystem {
        fn load_module(&self) -> Result<(), NbdError> {
            self.module_loaded.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn is_module_loaded(&self) -> bool {
            self.module_loaded.load(Ordering::SeqCst)
        }

        fn read_max_devices(&self) -> Option<usize> {
            Some(self.max_devices)
        }

        fn device_file_exists(&self, n: usize) -> bool {
            self.existing_devices.contains(&n)
        }
    }

    /// Fake system where modprobe always fails.
    struct FailingModprobe;

    impl NbdSystem for FailingModprobe {
        fn load_module(&self) -> Result<(), NbdError> {
            Err(NbdError::ModuleLoadFailed {
                reason: "modprobe not available in test".into(),
            })
        }

        fn is_module_loaded(&self) -> bool {
            false
        }

        fn read_max_devices(&self) -> Option<usize> {
            None
        }

        fn device_file_exists(&self, _n: usize) -> bool {
            false
        }
    }

    #[test]
    fn allocate_returns_first_free_device() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        let dev = alloc.allocate().unwrap();
        assert_eq!(dev.number, 0);
        assert_eq!(dev.path(), PathBuf::from("/dev/nbd0"));
    }

    #[test]
    fn allocate_skips_in_use_devices() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        let dev0 = alloc.allocate().unwrap();
        assert_eq!(dev0.number, 0);

        let dev1 = alloc.allocate().unwrap();
        assert_eq!(dev1.number, 1);

        let dev2 = alloc.allocate().unwrap();
        assert_eq!(dev2.number, 2);
    }

    #[test]
    fn allocate_skips_missing_device_files() {
        // Only device files 0 and 2 exist; 1 is missing.
        let sys = FakeSystem::new(4, &[0, 2]);
        let alloc = NbdAllocator::with_system(sys);

        let dev0 = alloc.allocate().unwrap();
        assert_eq!(dev0.number, 0);

        // Should skip nbd1 (missing) and return nbd2.
        let dev1 = alloc.allocate().unwrap();
        assert_eq!(dev1.number, 2);
    }

    #[test]
    fn allocate_all_busy_returns_error() {
        let sys = FakeSystem::new(2, &[0, 1]);
        let alloc = NbdAllocator::with_system(sys);

        alloc.allocate().unwrap(); // nbd0
        alloc.allocate().unwrap(); // nbd1

        let err = alloc.allocate().unwrap_err();
        assert!(
            matches!(err, NbdError::AllDevicesBusy { max: 2 }),
            "expected AllDevicesBusy, got: {err}"
        );
    }

    #[test]
    fn allocate_no_device_files_returns_error() {
        // max_devices = 4 but no device files exist at all.
        let sys = FakeSystem::new(4, &[]);
        let alloc = NbdAllocator::with_system(sys);

        let err = alloc.allocate().unwrap_err();
        assert!(
            matches!(err, NbdError::AllDevicesBusy { max: 4 }),
            "expected AllDevicesBusy, got: {err}"
        );
    }

    #[test]
    fn release_frees_slot_for_reallocation() {
        let sys = FakeSystem::new(2, &[0, 1]);
        let alloc = NbdAllocator::with_system(sys);

        let dev0 = alloc.allocate().unwrap();
        let _dev1 = alloc.allocate().unwrap();
        assert_eq!(alloc.allocated_count(), 2);

        alloc.release(&dev0).unwrap();
        assert_eq!(alloc.allocated_count(), 1);

        // Re-allocating should return nbd0 again (first free).
        let dev_realloc = alloc.allocate().unwrap();
        assert_eq!(dev_realloc.number, 0);
    }

    #[test]
    fn release_unallocated_device_returns_error() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        let fake_dev = NbdDevice { number: 5 };
        let err = alloc.release(&fake_dev).unwrap_err();
        assert!(
            matches!(err, NbdError::NotAllocated { n: 5 }),
            "expected NotAllocated, got: {err}"
        );
    }

    #[test]
    fn is_available_checks_existence_and_allocation() {
        let sys = FakeSystem::new(4, &[0, 1, 2]);
        let alloc = NbdAllocator::with_system(sys);

        // nbd0 exists and is free.
        assert!(alloc.is_available(0));

        // nbd3 does not have a device file.
        assert!(!alloc.is_available(3));

        // Allocate nbd0, now it should not be available.
        let _dev = alloc.allocate().unwrap();
        assert!(!alloc.is_available(0));
        assert!(alloc.is_available(1));
    }

    #[test]
    fn is_available_out_of_range() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        assert!(!alloc.is_available(4));
        assert!(!alloc.is_available(100));
    }

    #[test]
    fn max_devices_returns_configured_value() {
        let sys = FakeSystem::new(32, &[]);
        let alloc = NbdAllocator::with_system(sys);
        assert_eq!(alloc.max_devices(), 32);
    }

    #[test]
    fn max_devices_defaults_when_unreadable() {
        let alloc = NbdAllocator::with_system(FailingModprobe);
        assert_eq!(alloc.max_devices(), DEFAULT_MAX_DEVICES);
    }

    #[test]
    fn load_module_skips_if_already_loaded() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        // Module is already "loaded" in FakeSystem.
        assert!(alloc.load_module().is_ok());
    }

    #[test]
    fn load_module_calls_modprobe_when_not_loaded() {
        let sys = FakeSystem::with_module_unloaded(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        assert!(alloc.load_module().is_ok());
    }

    #[test]
    fn load_module_failure() {
        let alloc = NbdAllocator::with_system(FailingModprobe);

        let err = alloc.load_module().unwrap_err();
        assert!(
            matches!(err, NbdError::ModuleLoadFailed { .. }),
            "expected ModuleLoadFailed, got: {err}"
        );
    }

    #[test]
    fn allocated_devices_returns_sorted_list() {
        let sys = FakeSystem::new(8, &[0, 1, 2, 3, 4, 5, 6, 7]);
        let alloc = NbdAllocator::with_system(sys);

        alloc.allocate().unwrap(); // 0
        alloc.allocate().unwrap(); // 1
        alloc.allocate().unwrap(); // 2

        let devs = alloc.allocated_devices();
        assert_eq!(devs, vec![0, 1, 2]);
    }

    #[test]
    fn nbd_device_display() {
        let dev = NbdDevice { number: 7 };
        assert_eq!(format!("{dev}"), "/dev/nbd7");
    }

    #[test]
    fn double_release_returns_error() {
        let sys = FakeSystem::new(4, &[0, 1, 2, 3]);
        let alloc = NbdAllocator::with_system(sys);

        let dev = alloc.allocate().unwrap();
        alloc.release(&dev).unwrap();

        // Second release should fail.
        let err = alloc.release(&dev).unwrap_err();
        assert!(
            matches!(err, NbdError::NotAllocated { .. }),
            "expected NotAllocated on double release, got: {err}"
        );
    }

    #[test]
    fn concurrent_allocations_are_safe() {
        use std::sync::Arc;
        use std::thread;

        let sys = FakeSystem::new(64, &(0..64).collect::<Vec<_>>());
        let alloc = Arc::new(NbdAllocator::with_system(sys));

        let handles: Vec<_> = (0..16)
            .map(|_| {
                let alloc = Arc::clone(&alloc);
                thread::spawn(move || alloc.allocate().unwrap())
            })
            .collect();

        let mut numbers: Vec<usize> = handles
            .into_iter()
            .map(|h| h.join().unwrap().number)
            .collect();
        numbers.sort_unstable();
        numbers.dedup();

        // All 16 threads should have gotten unique device numbers.
        assert_eq!(numbers.len(), 16);
        assert_eq!(alloc.allocated_count(), 16);
    }
}
