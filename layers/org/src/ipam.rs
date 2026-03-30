//! IPAM — IP Address Management for subnet allocation.
//!
//! Bitmap allocator: 1 bit per IP in a /24 subnet (256 bits = 32 bytes).
//! Reserved IPs: .0 (network), .1 (gateway), .2 (reserved/DNS), .255 (broadcast).
//! Allocatable range: .3 to .254 (252 addresses).
//!
//! MAC addresses are derived deterministically from IP:
//! `02:00:{IP octets in hex}` (e.g., 10.0.1.5 → 02:00:0a:00:01:05).

use std::net::Ipv4Addr;

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};

const IPAM_BITMAPS_TABLE: &str = "ipam_bitmaps";

/// Number of allocatable IPs in a /24 subnet (.3 through .254).
pub const ALLOCATABLE_IPS: u32 = 252;

/// Reserved bit positions in a /24: .0, .1, .2, .255.
const RESERVED_POSITIONS: &[u8] = &[0, 1, 2, 255];

/// A 256-bit bitmap for tracking IP allocations in a /24 subnet.
///
/// Each bit corresponds to one IP in the subnet. Bit 0 = .0, bit 255 = .255.
/// A set bit means the IP is allocated (or reserved).
#[derive(Clone, Debug)]
pub struct SubnetBitmap {
    /// 32 bytes = 256 bits, one per IP in the /24.
    bits: [u8; 32],
}

impl Default for SubnetBitmap {
    fn default() -> Self {
        Self::new()
    }
}

impl SubnetBitmap {
    /// Create a new bitmap with reserved IPs pre-marked.
    pub fn new() -> Self {
        let mut bm = SubnetBitmap { bits: [0u8; 32] };
        for &pos in RESERVED_POSITIONS {
            bm.set(pos);
        }
        bm
    }

    /// Create a bitmap from raw bytes (for deserialization from the database).
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        SubnetBitmap { bits: bytes }
    }

    /// Get the raw bytes (for serialization to the database).
    pub fn to_bytes(&self) -> [u8; 32] {
        self.bits
    }

    /// Check if a bit position is set (allocated or reserved).
    fn is_set(&self, pos: u8) -> bool {
        let byte_idx = (pos / 8) as usize;
        let bit_idx = pos % 8;
        (self.bits[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Set a bit position (mark as allocated).
    fn set(&mut self, pos: u8) {
        let byte_idx = (pos / 8) as usize;
        let bit_idx = pos % 8;
        self.bits[byte_idx] |= 1 << bit_idx;
    }

    /// Clear a bit position (mark as free).
    fn clear(&mut self, pos: u8) {
        let byte_idx = (pos / 8) as usize;
        let bit_idx = pos % 8;
        self.bits[byte_idx] &= !(1 << bit_idx);
    }

    /// Find the first free (unset) bit in the allocatable range (.3 to .254).
    /// Returns `None` if all allocatable IPs are taken.
    pub fn first_free(&self) -> Option<u8> {
        (3..=254u8).find(|&pos| !self.is_set(pos))
    }

    /// Allocate the next available IP. Returns the host part (.3–.254).
    /// Returns `None` if no IPs are available.
    pub fn allocate(&mut self) -> Option<u8> {
        let pos = self.first_free()?;
        self.set(pos);
        Some(pos)
    }

    /// Release an IP by its host part. Returns true if the IP was allocated.
    /// Does not allow releasing reserved IPs (.0, .1, .2, .255).
    pub fn release(&mut self, pos: u8) -> bool {
        if RESERVED_POSITIONS.contains(&pos) {
            return false;
        }
        if self.is_set(pos) {
            self.clear(pos);
            true
        } else {
            false
        }
    }

    /// Count the number of free (unallocated) IPs in the allocatable range.
    pub fn available_count(&self) -> u32 {
        let mut count = 0u32;
        for pos in 3..=254u8 {
            if !self.is_set(pos) {
                count += 1;
            }
        }
        count
    }
}

/// Derive a deterministic MAC address from an IPv4 address.
///
/// Format: `02:00:{o1:02x}:{o2:02x}:{o3:02x}:{o4:02x}`
/// The `02` prefix marks it as a locally administered unicast MAC.
pub fn mac_from_ip(ip: Ipv4Addr) -> String {
    let o = ip.octets();
    format!("02:00:{:02x}:{:02x}:{:02x}:{:02x}", o[0], o[1], o[2], o[3])
}

/// Compute the full IPv4 address from a subnet base and host offset.
pub fn ip_from_subnet_and_offset(subnet_base: Ipv4Addr, offset: u8) -> Ipv4Addr {
    let mut octets = subnet_base.octets();
    octets[3] = offset;
    Ipv4Addr::from(octets)
}

/// IPAM store — manages bitmap persistence for subnet IP allocation.
pub struct IpamStore<'a> {
    db: &'a LayerDb,
}

impl<'a> IpamStore<'a> {
    /// Create a new IPAM store backed by the given database.
    pub fn new(db: &'a LayerDb) -> Self {
        Self { db }
    }

    /// Load the bitmap for a subnet, or create a fresh one if none exists.
    pub fn load_bitmap(&self, subnet_id: &str) -> Result<SubnetBitmap> {
        match self.db.get::<Vec<u8>>(IPAM_BITMAPS_TABLE, subnet_id)? {
            Some(bytes) => {
                if bytes.len() != 32 {
                    return Err(OrgError::StoreError(format!(
                        "corrupt IPAM bitmap for subnet {subnet_id}: expected 32 bytes, got {}",
                        bytes.len()
                    )));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(SubnetBitmap::from_bytes(arr))
            }
            None => Ok(SubnetBitmap::new()),
        }
    }

    /// Persist a bitmap for a subnet.
    pub fn save_bitmap(&self, subnet_id: &str, bitmap: &SubnetBitmap) -> Result<()> {
        let bytes = bitmap.to_bytes().to_vec();
        self.db.set(IPAM_BITMAPS_TABLE, subnet_id, &bytes)?;
        Ok(())
    }

    /// Allocate the next available IP from a subnet.
    ///
    /// Returns `(ip, mac)` on success, or `IpExhausted` if no IPs remain.
    pub fn allocate_ip(
        &self,
        subnet_id: &str,
        subnet_name: &str,
        subnet_base: Ipv4Addr,
    ) -> Result<(Ipv4Addr, String)> {
        let mut bitmap = self.load_bitmap(subnet_id)?;

        let offset = bitmap.allocate().ok_or_else(|| OrgError::IpExhausted {
            subnet_name: subnet_name.to_string(),
            available: 0,
            total: ALLOCATABLE_IPS,
        })?;

        self.save_bitmap(subnet_id, &bitmap)?;

        let ip = ip_from_subnet_and_offset(subnet_base, offset);
        let mac = mac_from_ip(ip);
        Ok((ip, mac))
    }

    /// Release an IP back to the subnet pool.
    ///
    /// The `offset` is the last octet of the IP (e.g., 5 for 10.0.1.5).
    pub fn release_ip(&self, subnet_id: &str, offset: u8) -> Result<bool> {
        let mut bitmap = self.load_bitmap(subnet_id)?;
        let released = bitmap.release(offset);
        if released {
            self.save_bitmap(subnet_id, &bitmap)?;
        }
        Ok(released)
    }

    /// Return the number of available (free) IPs in a subnet.
    pub fn available_count(&self, subnet_id: &str) -> Result<u32> {
        let bitmap = self.load_bitmap(subnet_id)?;
        Ok(bitmap.available_count())
    }

    /// Delete the bitmap for a subnet (when the subnet is deleted).
    pub fn delete_bitmap(&self, subnet_id: &str) -> Result<()> {
        self.db.delete(IPAM_BITMAPS_TABLE, subnet_id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Pure bitmap tests ────────────────────────────────────────────

    #[test]
    fn new_bitmap_reserves_special_ips() {
        let bm = SubnetBitmap::new();
        // .0, .1, .2, .255 should be set
        assert!(bm.is_set(0), ".0 (network) should be reserved");
        assert!(bm.is_set(1), ".1 (gateway) should be reserved");
        assert!(bm.is_set(2), ".2 (reserved) should be reserved");
        assert!(bm.is_set(255), ".255 (broadcast) should be reserved");
        // .3 should be free
        assert!(!bm.is_set(3), ".3 should be free");
    }

    #[test]
    fn allocate_first_ip() {
        let mut bm = SubnetBitmap::new();
        let pos = bm.allocate();
        assert_eq!(pos, Some(3), "first allocation should be .3");
    }

    #[test]
    fn allocate_sequential() {
        let mut bm = SubnetBitmap::new();
        assert_eq!(bm.allocate(), Some(3));
        assert_eq!(bm.allocate(), Some(4));
        assert_eq!(bm.allocate(), Some(5));
    }

    #[test]
    fn release_and_reallocate() {
        let mut bm = SubnetBitmap::new();
        let p1 = bm.allocate().unwrap(); // .3
        let _p2 = bm.allocate().unwrap(); // .4
        assert!(bm.release(p1), "release should succeed");
        // Re-allocate should give .3 back
        assert_eq!(bm.allocate(), Some(3));
    }

    #[test]
    fn skip_reserved_ips() {
        let bm = SubnetBitmap::new();
        // First free should be .3, not .0/.1/.2
        assert_eq!(bm.first_free(), Some(3));
    }

    #[test]
    fn cannot_release_reserved() {
        let mut bm = SubnetBitmap::new();
        assert!(!bm.release(0), "cannot release .0");
        assert!(!bm.release(1), "cannot release .1");
        assert!(!bm.release(2), "cannot release .2");
        assert!(!bm.release(255), "cannot release .255");
    }

    #[test]
    fn available_count_fresh() {
        let bm = SubnetBitmap::new();
        assert_eq!(bm.available_count(), 252);
    }

    #[test]
    fn available_count_after_alloc() {
        let mut bm = SubnetBitmap::new();
        bm.allocate();
        assert_eq!(bm.available_count(), 251);
    }

    #[test]
    fn exhaust_subnet() {
        let mut bm = SubnetBitmap::new();

        // Allocate all 252 IPs
        for i in 0..ALLOCATABLE_IPS {
            let pos = bm.allocate();
            assert!(
                pos.is_some(),
                "allocation {i} should succeed (expected .{})",
                i + 3
            );
        }

        assert_eq!(bm.available_count(), 0, "no IPs should remain");

        // Next allocation must fail
        assert_eq!(bm.allocate(), None, "allocation after exhaustion must fail");
    }

    #[test]
    fn bitmap_roundtrip() {
        let mut bm = SubnetBitmap::new();
        bm.allocate(); // .3
        bm.allocate(); // .4
        let bytes = bm.to_bytes();
        let bm2 = SubnetBitmap::from_bytes(bytes);
        assert_eq!(bm2.first_free(), Some(5));
        assert_eq!(bm2.available_count(), 250);
    }

    #[test]
    fn mac_derivation() {
        let ip = Ipv4Addr::new(10, 0, 1, 5);
        assert_eq!(mac_from_ip(ip), "02:00:0a:00:01:05");
    }

    // ── Store-backed tests ───────────────────────────────────────────

    fn temp_ipam() -> (tempfile::TempDir, LayerDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-test.redb");
        let db = LayerDb::open_at(&path).unwrap();
        (dir, db)
    }

    #[test]
    fn store_allocate_and_persist() {
        let (_dir, db) = temp_ipam();
        let store = IpamStore::new(&db);

        let (ip, mac) = store
            .allocate_ip("test-subnet", "frontend", Ipv4Addr::new(10, 1, 1, 0))
            .unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 1, 1, 3));
        assert_eq!(mac, "02:00:0a:01:01:03");

        // Verify persistence — reload bitmap and allocate the next one
        let (ip2, _) = store
            .allocate_ip("test-subnet", "frontend", Ipv4Addr::new(10, 1, 1, 0))
            .unwrap();
        assert_eq!(ip2, Ipv4Addr::new(10, 1, 1, 4));
    }

    #[test]
    fn store_release_ip() {
        let (_dir, db) = temp_ipam();
        let store = IpamStore::new(&db);

        let (ip, _) = store
            .allocate_ip("test-subnet", "frontend", Ipv4Addr::new(10, 1, 1, 0))
            .unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 1, 1, 3));

        assert!(store.release_ip("test-subnet", 3).unwrap());
        assert_eq!(store.available_count("test-subnet").unwrap(), 252);
    }

    #[test]
    fn store_available_count() {
        let (_dir, db) = temp_ipam();
        let store = IpamStore::new(&db);

        assert_eq!(store.available_count("nonexistent").unwrap(), 252);

        store
            .allocate_ip("s1", "frontend", Ipv4Addr::new(10, 1, 1, 0))
            .unwrap();
        assert_eq!(store.available_count("s1").unwrap(), 251);
    }

    #[test]
    fn store_exhaust_returns_ip_exhausted() {
        let (_dir, db) = temp_ipam();
        let store = IpamStore::new(&db);

        let subnet_base = Ipv4Addr::new(10, 1, 1, 0);

        // Allocate all 252 IPs
        for _ in 0..ALLOCATABLE_IPS {
            store.allocate_ip("s1", "frontend", subnet_base).unwrap();
        }

        // Next must return IpExhausted
        let err = store
            .allocate_ip("s1", "frontend", subnet_base)
            .unwrap_err();
        match &err {
            OrgError::IpExhausted {
                subnet_name,
                available,
                total,
            } => {
                assert_eq!(subnet_name, "frontend");
                assert_eq!(*available, 0);
                assert_eq!(*total, ALLOCATABLE_IPS);
            }
            other => panic!("expected IpExhausted, got: {other}"),
        }
    }

    #[test]
    fn error_message_actionable() {
        let err = OrgError::IpExhausted {
            subnet_name: "frontend".to_string(),
            available: 0,
            total: 252,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("frontend"),
            "error must mention subnet name: {msg}"
        );
        assert!(
            msg.contains("0/252"),
            "error must show available/total: {msg}"
        );
        assert!(
            msg.to_lowercase().contains("create a new subnet"),
            "error must suggest creating a new subnet: {msg}"
        );
    }
}
