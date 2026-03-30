//! IPAM — IP Address Management with bitmap allocation and lifecycle tracking.
//!
//! The bitmap is the fast-path allocator (1 bit per IP). The `ip_allocations`
//! table is the audit trail that tracks the full lifecycle of each allocation:
//! Reserved -> Assigned -> released (deleted).
//!
//! Orphan detection catches IPs that were reserved but never assigned (e.g.,
//! VM creation failed between IPAM allocation and boot).

use std::fmt;
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};

// ── Constants ────────────────────────────────────────────────────────

const IPAM_BITMAPS_TABLE: &str = "ipam_bitmaps";
const IP_ALLOCATIONS_TABLE: &str = "ip_allocations";

/// Number of IPs in a /24 subnet.
const SUBNET_SIZE: usize = 256;

/// Bitmap size in bytes for a /24 (256 bits = 32 bytes).
const BITMAP_BYTES: usize = SUBNET_SIZE / 8;

/// Reserved host offsets: .0 (network), .1 (gateway), .2 (reserved/DNS), .255 (broadcast).
const RESERVED_OFFSETS: &[u8] = &[0, 1, 2, 255];

/// First allocatable host offset.
const FIRST_ALLOCATABLE: u8 = 3;

/// Last allocatable host offset.
const LAST_ALLOCATABLE: u8 = 254;

/// Default orphan threshold: 5 minutes (300 seconds).
pub const DEFAULT_ORPHAN_THRESHOLD_SECS: u64 = 300;

// ── Types ────────────────────────────────────────────────────────────

/// State of an IP allocation in its lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationState {
    /// IP reserved from bitmap but not yet assigned to a VM.
    Reserved,
    /// IP assigned to a running VM.
    Assigned,
    /// IP was reserved but never assigned within the orphan threshold.
    Orphaned,
}

impl fmt::Display for AllocationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AllocationState::Reserved => f.write_str("Reserved"),
            AllocationState::Assigned => f.write_str("Assigned"),
            AllocationState::Orphaned => f.write_str("Orphaned"),
        }
    }
}

/// A tracked IP allocation with full lifecycle metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpAllocation {
    /// The allocated IP address (e.g., "10.0.1.3").
    pub ip: String,
    /// The subnet this IP belongs to.
    pub subnet_id: String,
    /// The VM using this IP. None if Reserved (not yet assigned).
    pub vm_id: Option<String>,
    /// MAC address derived deterministically from the IP.
    pub mac: String,
    /// Current lifecycle state.
    pub state: AllocationState,
    /// Unix timestamp when the IP was reserved from the bitmap.
    pub allocated_at: u64,
    /// Unix timestamp when the IP was assigned to a VM. None if not yet assigned.
    pub assigned_at: Option<u64>,
}

// ── Bitmap allocator ─────────────────────────────────────────────────

/// A /24 subnet bitmap: 256 bits (32 bytes), one bit per IP.
/// Bit set = IP in use. Bit clear = IP available.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubnetBitmap {
    /// Raw bitmap bytes. Index 0 bit 0 = .0, index 0 bit 1 = .1, etc.
    pub bits: Vec<u8>,
}

impl SubnetBitmap {
    /// Create a new bitmap with reserved addresses pre-marked.
    pub fn new() -> Self {
        let mut bits = vec![0u8; BITMAP_BYTES];
        for &offset in RESERVED_OFFSETS {
            Self::set_bit(&mut bits, offset as usize);
        }
        Self { bits }
    }

    /// Set a bit (mark IP as in-use).
    fn set_bit(bits: &mut [u8], offset: usize) {
        let byte_idx = offset / 8;
        let bit_idx = offset % 8;
        if byte_idx < bits.len() {
            bits[byte_idx] |= 1 << bit_idx;
        }
    }

    /// Clear a bit (mark IP as available).
    fn clear_bit(bits: &mut [u8], offset: usize) {
        let byte_idx = offset / 8;
        let bit_idx = offset % 8;
        if byte_idx < bits.len() {
            bits[byte_idx] &= !(1 << bit_idx);
        }
    }

    /// Check if a bit is set.
    fn is_set(bits: &[u8], offset: usize) -> bool {
        let byte_idx = offset / 8;
        let bit_idx = offset % 8;
        if byte_idx < bits.len() {
            (bits[byte_idx] >> bit_idx) & 1 == 1
        } else {
            false
        }
    }

    /// Allocate the first available IP. Returns the host offset (3..=254).
    pub fn allocate(&mut self) -> Option<u8> {
        for offset in FIRST_ALLOCATABLE..=LAST_ALLOCATABLE {
            if !Self::is_set(&self.bits, offset as usize) {
                Self::set_bit(&mut self.bits, offset as usize);
                return Some(offset);
            }
        }
        None
    }

    /// Release an IP by its host offset.
    pub fn release(&mut self, offset: u8) {
        // Never release reserved addresses.
        if RESERVED_OFFSETS.contains(&offset) {
            return;
        }
        Self::clear_bit(&mut self.bits, offset as usize);
    }

    /// Check if a specific offset is allocated.
    pub fn is_allocated(&self, offset: u8) -> bool {
        Self::is_set(&self.bits, offset as usize)
    }

    /// Count the number of available (allocatable) IPs.
    pub fn available_count(&self) -> u32 {
        let mut count = 0u32;
        for offset in FIRST_ALLOCATABLE..=LAST_ALLOCATABLE {
            if !Self::is_set(&self.bits, offset as usize) {
                count += 1;
            }
        }
        count
    }
}

impl Default for SubnetBitmap {
    fn default() -> Self {
        Self::new()
    }
}

// ── MAC derivation ───────────────────────────────────────────────────

/// Derive a MAC address deterministically from an IPv4 address.
///
/// Format: `02:00:{octet1:02x}:{octet2:02x}:{octet3:02x}:{octet4:02x}`
///
/// The `02` prefix sets the locally-administered bit, avoiding conflicts
/// with globally-assigned OUI ranges.
pub fn mac_from_ip(ip: &Ipv4Addr) -> String {
    let o = ip.octets();
    format!("02:00:{:02x}:{:02x}:{:02x}:{:02x}", o[0], o[1], o[2], o[3])
}

// ── Helper: parse subnet base from CIDR string ──────────────────────

/// Extract the network base address from a CIDR string like "10.0.1.0/24".
fn parse_subnet_base(cidr: &str) -> Result<Ipv4Addr> {
    let net: ipnet::Ipv4Net = cidr
        .parse()
        .map_err(|_| OrgError::InvalidCidr(format!("invalid subnet CIDR: {cidr}")))?;
    Ok(net.network())
}

/// Build an IP address from a subnet base and a host offset.
fn ip_from_offset(base: &Ipv4Addr, offset: u8) -> Ipv4Addr {
    let mut octets = base.octets();
    octets[3] = offset;
    Ipv4Addr::from(octets)
}

// ── Current time helper ──────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── IPAM store ───────────────────────────────────────────────────────

/// IP Address Management store backed by redb.
///
/// Manages bitmap allocation for fast IP assignment and an allocation
/// table for lifecycle tracking and orphan detection.
pub struct IpamStore {
    db: LayerDb,
}

impl IpamStore {
    /// Create a new IPAM store with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Build the allocation table key: "subnet_id/ip".
    fn allocation_key(subnet_id: &str, ip: &str) -> String {
        format!("{subnet_id}/{ip}")
    }

    /// Load or create the bitmap for a subnet.
    fn load_bitmap(&self, subnet_id: &str) -> Result<SubnetBitmap> {
        match self.db.get::<SubnetBitmap>(IPAM_BITMAPS_TABLE, subnet_id)? {
            Some(bm) => Ok(bm),
            None => Ok(SubnetBitmap::new()),
        }
    }

    /// Persist the bitmap for a subnet (used by callers that don't need
    /// atomic batch writes).
    #[allow(dead_code)]
    fn save_bitmap(&self, subnet_id: &str, bitmap: &SubnetBitmap) -> Result<()> {
        self.db.set(IPAM_BITMAPS_TABLE, subnet_id, bitmap)?;
        Ok(())
    }

    /// Reserve an IP from a subnet's bitmap.
    ///
    /// Allocates the next available IP, creates an `IpAllocation` record
    /// with `state = Reserved`, and returns it. The caller is responsible
    /// for later calling `assign_ip` once the VM is booted, or `release_ip`
    /// on failure.
    ///
    /// `subnet_cidr` is the CIDR string (e.g., "10.0.1.0/24") used to
    /// compute the full IP address from the bitmap offset.
    pub fn reserve_ip(&self, subnet_id: &str, subnet_cidr: &str) -> Result<IpAllocation> {
        let base = parse_subnet_base(subnet_cidr)?;
        let mut bitmap = self.load_bitmap(subnet_id)?;

        let offset = bitmap.allocate().ok_or(OrgError::IpExhausted {
            subnet: subnet_id.to_string(),
        })?;

        let ip = ip_from_offset(&base, offset);
        let mac = mac_from_ip(&ip);
        let now = now_secs();

        let allocation = IpAllocation {
            ip: ip.to_string(),
            subnet_id: subnet_id.to_string(),
            vm_id: None,
            mac,
            state: AllocationState::Reserved,
            allocated_at: now,
            assigned_at: None,
        };

        // Persist bitmap and allocation atomically.
        let key = Self::allocation_key(subnet_id, &allocation.ip);
        self.db.batch(|w| {
            w.set(IPAM_BITMAPS_TABLE, subnet_id, &bitmap)?;
            w.set(IP_ALLOCATIONS_TABLE, &key, &allocation)?;
            Ok(())
        })?;

        Ok(allocation)
    }

    /// Reserve an IP from a subnet using a custom timestamp (for testing).
    pub fn reserve_ip_at(
        &self,
        subnet_id: &str,
        subnet_cidr: &str,
        timestamp: u64,
    ) -> Result<IpAllocation> {
        let base = parse_subnet_base(subnet_cidr)?;
        let mut bitmap = self.load_bitmap(subnet_id)?;

        let offset = bitmap.allocate().ok_or(OrgError::IpExhausted {
            subnet: subnet_id.to_string(),
        })?;

        let ip = ip_from_offset(&base, offset);
        let mac = mac_from_ip(&ip);

        let allocation = IpAllocation {
            ip: ip.to_string(),
            subnet_id: subnet_id.to_string(),
            vm_id: None,
            mac,
            state: AllocationState::Reserved,
            allocated_at: timestamp,
            assigned_at: None,
        };

        let key = Self::allocation_key(subnet_id, &allocation.ip);
        self.db.batch(|w| {
            w.set(IPAM_BITMAPS_TABLE, subnet_id, &bitmap)?;
            w.set(IP_ALLOCATIONS_TABLE, &key, &allocation)?;
            Ok(())
        })?;

        Ok(allocation)
    }

    /// Assign a reserved IP to a VM.
    ///
    /// Transitions the allocation from `Reserved` to `Assigned`, sets the
    /// `vm_id` and `assigned_at` timestamp.
    pub fn assign_ip(&self, subnet_id: &str, ip: &str, vm_id: &str) -> Result<IpAllocation> {
        let key = Self::allocation_key(subnet_id, ip);
        let mut allocation: IpAllocation =
            self.db
                .get(IP_ALLOCATIONS_TABLE, &key)?
                .ok_or(OrgError::IpNotFound {
                    subnet: subnet_id.to_string(),
                    ip: ip.to_string(),
                })?;

        if allocation.state == AllocationState::Assigned {
            return Err(OrgError::IpAlreadyAssigned {
                subnet: subnet_id.to_string(),
                ip: ip.to_string(),
            });
        }

        allocation.state = AllocationState::Assigned;
        allocation.vm_id = Some(vm_id.to_string());
        allocation.assigned_at = Some(now_secs());

        self.db.set(IP_ALLOCATIONS_TABLE, &key, &allocation)?;
        Ok(allocation)
    }

    /// Release an IP: clear the bitmap bit and delete the allocation record.
    ///
    /// This is called on VM delete or when reclaiming an orphaned allocation.
    pub fn release_ip(&self, subnet_id: &str, subnet_cidr: &str, ip: &str) -> Result<()> {
        let _base = parse_subnet_base(subnet_cidr)?;
        let release_ip: Ipv4Addr = ip
            .parse()
            .map_err(|_| OrgError::InvalidCidr(format!("invalid IP address: {ip}")))?;

        // Compute offset from base.
        let offset = release_ip.octets()[3];
        if !(FIRST_ALLOCATABLE..=LAST_ALLOCATABLE).contains(&offset) {
            return Err(OrgError::InvalidCidr(format!(
                "cannot release reserved address: {ip}"
            )));
        }

        let mut bitmap = self.load_bitmap(subnet_id)?;
        bitmap.release(offset);

        let alloc_key = Self::allocation_key(subnet_id, ip);

        // Verify the allocation exists before deleting.
        let exists = self.db.exists(IP_ALLOCATIONS_TABLE, &alloc_key)?;
        if !exists {
            return Err(OrgError::IpNotFound {
                subnet: subnet_id.to_string(),
                ip: ip.to_string(),
            });
        }

        self.db.batch(|w| {
            w.set(IPAM_BITMAPS_TABLE, subnet_id, &bitmap)?;
            w.delete(IP_ALLOCATIONS_TABLE, &alloc_key)?;
            Ok(())
        })?;

        Ok(())
    }

    /// List all IP allocations in a subnet.
    pub fn list_allocations(&self, subnet_id: &str) -> Result<Vec<IpAllocation>> {
        let all: Vec<(String, IpAllocation)> = self.db.list(IP_ALLOCATIONS_TABLE)?;
        let prefix = format!("{subnet_id}/");
        Ok(all
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, alloc)| alloc)
            .collect())
    }

    /// Find orphaned allocations: IPs in `Reserved` state older than `max_age_secs`.
    ///
    /// An orphaned IP is one that was reserved from the bitmap but never
    /// assigned to a VM — typically because VM creation failed between
    /// the IPAM reservation and the VM boot.
    pub fn find_orphans(&self, max_age_secs: u64) -> Result<Vec<IpAllocation>> {
        let now = now_secs();
        let all: Vec<(String, IpAllocation)> = self.db.list(IP_ALLOCATIONS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, alloc)| {
                alloc.state == AllocationState::Reserved
                    && now.saturating_sub(alloc.allocated_at) > max_age_secs
            })
            .map(|(_, alloc)| alloc)
            .collect())
    }

    /// Find orphaned allocations using a custom "now" timestamp (for testing).
    pub fn find_orphans_at(&self, max_age_secs: u64, now: u64) -> Result<Vec<IpAllocation>> {
        let all: Vec<(String, IpAllocation)> = self.db.list(IP_ALLOCATIONS_TABLE)?;
        Ok(all
            .into_iter()
            .filter(|(_, alloc)| {
                alloc.state == AllocationState::Reserved
                    && now.saturating_sub(alloc.allocated_at) > max_age_secs
            })
            .map(|(_, alloc)| alloc)
            .collect())
    }

    /// Mark an allocation as orphaned (without releasing it yet).
    ///
    /// This is useful for logging/alerting before reclamation.
    pub fn mark_orphaned(&self, subnet_id: &str, ip: &str) -> Result<IpAllocation> {
        let key = Self::allocation_key(subnet_id, ip);
        let mut allocation: IpAllocation =
            self.db
                .get(IP_ALLOCATIONS_TABLE, &key)?
                .ok_or(OrgError::IpNotFound {
                    subnet: subnet_id.to_string(),
                    ip: ip.to_string(),
                })?;

        allocation.state = AllocationState::Orphaned;
        self.db.set(IP_ALLOCATIONS_TABLE, &key, &allocation)?;
        Ok(allocation)
    }

    /// Get a single allocation by subnet and IP.
    pub fn get_allocation(&self, subnet_id: &str, ip: &str) -> Result<Option<IpAllocation>> {
        let key = Self::allocation_key(subnet_id, ip);
        Ok(self.db.get(IP_ALLOCATIONS_TABLE, &key)?)
    }

    /// Get the bitmap for a subnet (for inspection/debugging).
    pub fn get_bitmap(&self, subnet_id: &str) -> Result<Option<SubnetBitmap>> {
        Ok(self.db.get(IPAM_BITMAPS_TABLE, subnet_id)?)
    }
}

// ── Unit tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SUBNET_ID: &str = "default/frontend";
    const TEST_SUBNET_CIDR: &str = "10.0.1.0/24";

    fn temp_ipam() -> (tempfile::TempDir, IpamStore) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, IpamStore::new(db))
    }

    // ── Bitmap unit tests ────────────────────────────────────────────

    #[test]
    fn bitmap_reserved_addresses_pre_marked() {
        let bm = SubnetBitmap::new();
        assert!(bm.is_allocated(0), ".0 (network) must be reserved");
        assert!(bm.is_allocated(1), ".1 (gateway) must be reserved");
        assert!(bm.is_allocated(2), ".2 (reserved/DNS) must be reserved");
        assert!(bm.is_allocated(255), ".255 (broadcast) must be reserved");
        assert!(!bm.is_allocated(3), ".3 should be available");
        assert!(!bm.is_allocated(254), ".254 should be available");
    }

    #[test]
    fn bitmap_allocate_sequential() {
        let mut bm = SubnetBitmap::new();
        assert_eq!(bm.allocate(), Some(3));
        assert_eq!(bm.allocate(), Some(4));
        assert_eq!(bm.allocate(), Some(5));
    }

    #[test]
    fn bitmap_release_and_reallocate() {
        let mut bm = SubnetBitmap::new();
        let first = bm.allocate().unwrap();
        assert_eq!(first, 3);

        bm.release(first);
        assert!(!bm.is_allocated(3));

        // Re-allocating should return the freed offset.
        let reused = bm.allocate().unwrap();
        assert_eq!(reused, 3);
    }

    #[test]
    fn bitmap_cannot_release_reserved() {
        let mut bm = SubnetBitmap::new();
        bm.release(0);
        bm.release(1);
        bm.release(2);
        bm.release(255);
        assert!(bm.is_allocated(0));
        assert!(bm.is_allocated(1));
        assert!(bm.is_allocated(2));
        assert!(bm.is_allocated(255));
    }

    #[test]
    fn bitmap_exhaustion() {
        let mut bm = SubnetBitmap::new();
        // Allocate all 252 available IPs (3..=254).
        for i in 0..252 {
            assert!(
                bm.allocate().is_some(),
                "should allocate IP #{i} (offset {})",
                i + 3
            );
        }
        assert_eq!(bm.allocate(), None, "bitmap should be exhausted");
        assert_eq!(bm.available_count(), 0);
    }

    #[test]
    fn bitmap_available_count() {
        let mut bm = SubnetBitmap::new();
        assert_eq!(bm.available_count(), 252); // 3..=254
        bm.allocate();
        assert_eq!(bm.available_count(), 251);
    }

    // ── MAC derivation ───────────────────────────────────────────────

    #[test]
    fn mac_derivation() {
        let ip: Ipv4Addr = "10.0.1.5".parse().unwrap();
        assert_eq!(mac_from_ip(&ip), "02:00:0a:00:01:05");

        let ip2: Ipv4Addr = "192.168.255.1".parse().unwrap();
        assert_eq!(mac_from_ip(&ip2), "02:00:c0:a8:ff:01");
    }

    // ── Allocation lifecycle ─────────────────────────────────────────

    #[test]
    fn allocation_lifecycle() {
        let (_dir, store) = temp_ipam();

        // Reserve an IP.
        let alloc = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();
        assert_eq!(alloc.ip, "10.0.1.3");
        assert_eq!(alloc.state, AllocationState::Reserved);
        assert!(alloc.vm_id.is_none());
        assert!(alloc.assigned_at.is_none());
        assert_eq!(alloc.mac, "02:00:0a:00:01:03");

        // Assign to a VM.
        let assigned = store
            .assign_ip(TEST_SUBNET_ID, &alloc.ip, "vm-web-1")
            .unwrap();
        assert_eq!(assigned.state, AllocationState::Assigned);
        assert_eq!(assigned.vm_id, Some("vm-web-1".to_string()));
        assert!(assigned.assigned_at.is_some());

        // Verify it shows in list.
        let allocations = store.list_allocations(TEST_SUBNET_ID).unwrap();
        assert_eq!(allocations.len(), 1);
        assert_eq!(allocations[0].state, AllocationState::Assigned);

        // Release the IP.
        store
            .release_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR, &alloc.ip)
            .unwrap();

        // Verify allocation is gone.
        let allocations = store.list_allocations(TEST_SUBNET_ID).unwrap();
        assert!(allocations.is_empty());

        // Verify IP is available again in the bitmap.
        let bm = store.get_bitmap(TEST_SUBNET_ID).unwrap().unwrap();
        assert!(!bm.is_allocated(3));
    }

    #[test]
    fn orphan_detected() {
        let (_dir, store) = temp_ipam();

        // Reserve at a past time (10 minutes ago).
        let past = now_secs() - 600;
        let alloc = store
            .reserve_ip_at(TEST_SUBNET_ID, TEST_SUBNET_CIDR, past)
            .unwrap();
        assert_eq!(alloc.state, AllocationState::Reserved);

        // Find orphans with 5-minute threshold.
        let orphans = store
            .find_orphans_at(DEFAULT_ORPHAN_THRESHOLD_SECS, now_secs())
            .unwrap();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].ip, "10.0.1.3");
        assert_eq!(orphans[0].state, AllocationState::Reserved);
    }

    #[test]
    fn orphan_reclaimed() {
        let (_dir, store) = temp_ipam();

        // Reserve at a past time.
        let past = now_secs() - 600;
        let alloc = store
            .reserve_ip_at(TEST_SUBNET_ID, TEST_SUBNET_CIDR, past)
            .unwrap();

        // Detect the orphan.
        let orphans = store
            .find_orphans_at(DEFAULT_ORPHAN_THRESHOLD_SECS, now_secs())
            .unwrap();
        assert_eq!(orphans.len(), 1);

        // Mark it as orphaned.
        let marked = store.mark_orphaned(TEST_SUBNET_ID, &alloc.ip).unwrap();
        assert_eq!(marked.state, AllocationState::Orphaned);

        // Release the orphaned IP.
        store
            .release_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR, &alloc.ip)
            .unwrap();

        // Verify IP is available again.
        let bm = store.get_bitmap(TEST_SUBNET_ID).unwrap().unwrap();
        assert!(!bm.is_allocated(3));

        // Re-allocate — should get the same IP back.
        let new_alloc = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();
        assert_eq!(new_alloc.ip, "10.0.1.3");
    }

    #[test]
    fn crash_between_alloc_and_assign() {
        let (_dir, store) = temp_ipam();

        // Simulate: reserve IP, then "crash" (never call assign_ip).
        let past = now_secs() - 600;
        let alloc = store
            .reserve_ip_at(TEST_SUBNET_ID, TEST_SUBNET_CIDR, past)
            .unwrap();
        assert_eq!(alloc.state, AllocationState::Reserved);

        // After recovery, the reconciliation loop finds orphans.
        let orphans = store
            .find_orphans_at(DEFAULT_ORPHAN_THRESHOLD_SECS, now_secs())
            .unwrap();
        assert_eq!(orphans.len(), 1, "expected 1 orphan after simulated crash");
        assert_eq!(orphans[0].ip, alloc.ip);

        // Reclaim: release the orphan.
        store
            .release_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR, &alloc.ip)
            .unwrap();

        // Verify the IP is free.
        let bm = store.get_bitmap(TEST_SUBNET_ID).unwrap().unwrap();
        assert!(
            !bm.is_allocated(3),
            "IP should be free after orphan reclamation"
        );

        // No more orphans.
        let orphans = store
            .find_orphans_at(DEFAULT_ORPHAN_THRESHOLD_SECS, now_secs())
            .unwrap();
        assert!(orphans.is_empty(), "no orphans after reclamation");
    }

    #[test]
    fn recently_reserved_not_orphaned() {
        let (_dir, store) = temp_ipam();

        // Reserve at current time — should NOT be detected as orphan.
        let _alloc = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();

        let orphans = store.find_orphans(DEFAULT_ORPHAN_THRESHOLD_SECS).unwrap();
        assert!(orphans.is_empty(), "a freshly reserved IP is not an orphan");
    }

    #[test]
    fn assigned_ip_not_orphaned() {
        let (_dir, store) = temp_ipam();

        // Reserve at a past time, then assign — should NOT be orphaned.
        let past = now_secs() - 600;
        let alloc = store
            .reserve_ip_at(TEST_SUBNET_ID, TEST_SUBNET_CIDR, past)
            .unwrap();
        store
            .assign_ip(TEST_SUBNET_ID, &alloc.ip, "vm-web-1")
            .unwrap();

        let orphans = store
            .find_orphans_at(DEFAULT_ORPHAN_THRESHOLD_SECS, now_secs())
            .unwrap();
        assert!(orphans.is_empty(), "assigned IPs are not orphans");
    }

    #[test]
    fn double_assign_rejected() {
        let (_dir, store) = temp_ipam();

        let alloc = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();
        store.assign_ip(TEST_SUBNET_ID, &alloc.ip, "vm-1").unwrap();

        // Second assign should fail.
        let result = store.assign_ip(TEST_SUBNET_ID, &alloc.ip, "vm-2");
        assert!(result.is_err());
    }

    #[test]
    fn release_nonexistent_ip_fails() {
        let (_dir, store) = temp_ipam();

        let result = store.release_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR, "10.0.1.99");
        assert!(result.is_err());
    }

    #[test]
    fn multiple_subnets_isolated() {
        let (_dir, store) = temp_ipam();

        let alloc_a = store.reserve_ip("vpc/subnet-a", "10.0.1.0/24").unwrap();
        let alloc_b = store.reserve_ip("vpc/subnet-b", "10.0.2.0/24").unwrap();

        assert_eq!(alloc_a.ip, "10.0.1.3");
        assert_eq!(alloc_b.ip, "10.0.2.3");

        let list_a = store.list_allocations("vpc/subnet-a").unwrap();
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_a[0].subnet_id, "vpc/subnet-a");

        let list_b = store.list_allocations("vpc/subnet-b").unwrap();
        assert_eq!(list_b.len(), 1);
        assert_eq!(list_b[0].subnet_id, "vpc/subnet-b");
    }

    #[test]
    fn sequential_allocation_fills_subnet() {
        let (_dir, store) = temp_ipam();

        // Allocate a few IPs and verify sequential assignment.
        let a1 = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();
        let a2 = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();
        let a3 = store.reserve_ip(TEST_SUBNET_ID, TEST_SUBNET_CIDR).unwrap();

        assert_eq!(a1.ip, "10.0.1.3");
        assert_eq!(a2.ip, "10.0.1.4");
        assert_eq!(a3.ip, "10.0.1.5");
    }
}
