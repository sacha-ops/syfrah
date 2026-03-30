//! IPAM — IP Address Management with bitmap allocator.
//!
//! Each /24 subnet gets a 256-bit bitmap (32 bytes). Bit N = 1 means IP .N is allocated.
//! Reserved IPs (.0 network, .1 gateway, .2 DNS, .255 broadcast) are pre-set on creation.

use std::net::Ipv4Addr;

use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::{Subnet, SubnetId};

const IPAM_BITMAPS_TABLE: &str = "ipam_bitmaps";

/// Number of bytes in a /24 bitmap (256 bits = 32 bytes).
const BITMAP_SIZE: usize = 32;

/// Reserved bit indices: .0 (network), .1 (gateway), .2 (DNS), .255 (broadcast).
const RESERVED_INDICES: [u8; 4] = [0, 1, 2, 255];

/// A 256-bit bitmap for a /24 subnet stored as 32 bytes.
///
/// Bit N corresponds to IP offset N within the subnet. A set bit means the IP is allocated.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IpamBitmap {
    /// Raw bitmap bytes — 32 bytes = 256 bits.
    pub bits: Vec<u8>,
}

impl Default for IpamBitmap {
    fn default() -> Self {
        Self::new()
    }
}

impl IpamBitmap {
    /// Create a new bitmap with reserved IPs pre-set.
    pub fn new() -> Self {
        let mut bits = vec![0u8; BITMAP_SIZE];
        for &idx in &RESERVED_INDICES {
            Self::set_bit(&mut bits, idx);
        }
        Self { bits }
    }

    /// Set bit at the given index.
    fn set_bit(bits: &mut [u8], index: u8) {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        bits[byte_idx] |= 1 << bit_idx;
    }

    /// Clear bit at the given index.
    fn clear_bit(bits: &mut [u8], index: u8) {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        bits[byte_idx] &= !(1 << bit_idx);
    }

    /// Check if bit at the given index is set.
    fn is_bit_set(bits: &[u8], index: u8) -> bool {
        let byte_idx = (index / 8) as usize;
        let bit_idx = index % 8;
        (bits[byte_idx] & (1 << bit_idx)) != 0
    }

    /// Find the first free bit (unset), skipping reserved indices.
    /// Returns None if all 256 bits are set.
    pub fn find_first_free(&self) -> Option<u8> {
        for i in 0u16..256 {
            let idx = i as u8;
            if !Self::is_bit_set(&self.bits, idx) {
                return Some(idx);
            }
        }
        None
    }

    /// Mark a bit as allocated.
    pub fn allocate(&mut self, index: u8) {
        Self::set_bit(&mut self.bits, index);
    }

    /// Mark a bit as free.
    pub fn release(&mut self, index: u8) {
        Self::clear_bit(&mut self.bits, index);
    }

    /// Check if a bit is allocated.
    pub fn is_allocated(&self, index: u8) -> bool {
        Self::is_bit_set(&self.bits, index)
    }

    /// Count available (unset) bits.
    pub fn available_count(&self) -> u32 {
        let mut count = 0u32;
        for i in 0u16..256 {
            if !Self::is_bit_set(&self.bits, i as u8) {
                count += 1;
            }
        }
        count
    }
}

/// Parse the base network address from a subnet's CIDR string (e.g. "10.0.1.0/24" -> 10.0.1.0).
fn parse_subnet_base(subnet: &Subnet) -> Result<Ipv4Addr> {
    let cidr = &subnet.cidr;
    let addr_str = cidr
        .split('/')
        .next()
        .ok_or_else(|| OrgError::InvalidCidr(cidr.clone()))?;
    addr_str
        .parse::<Ipv4Addr>()
        .map_err(|_| OrgError::InvalidCidr(cidr.clone()))
}

/// Compute an IPv4 address from a subnet base and a bit index.
/// e.g. base=10.0.1.0, index=5 -> 10.0.1.5
fn ip_from_index(base: Ipv4Addr, index: u8) -> Ipv4Addr {
    let mut octets = base.octets();
    octets[3] = index;
    Ipv4Addr::from(octets)
}

/// Compute the bit index from a subnet base and an IP address.
/// Returns an error if the IP is not in the subnet's /24 range.
fn index_from_ip(base: Ipv4Addr, ip: Ipv4Addr) -> Result<u8> {
    let base_octets = base.octets();
    let ip_octets = ip.octets();
    if base_octets[0] != ip_octets[0]
        || base_octets[1] != ip_octets[1]
        || base_octets[2] != ip_octets[2]
    {
        return Err(OrgError::IpNotInSubnet {
            ip: ip.to_string(),
            subnet: format!(
                "{}.{}.{}.0/24",
                base_octets[0], base_octets[1], base_octets[2]
            ),
        });
    }
    Ok(ip_octets[3])
}

/// Check if an index is a reserved IP (.0, .1, .2, .255).
fn is_reserved(index: u8) -> bool {
    RESERVED_INDICES.contains(&index)
}

/// IPAM allocator backed by redb.
///
/// Manages IP allocation within /24 subnets using bitmaps persisted in the `ipam_bitmaps` table.
pub struct IpamAllocator {
    db: LayerDb,
}

impl IpamAllocator {
    /// Create a new allocator with the given database handle.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Storage key for a subnet's bitmap.
    fn bitmap_key(subnet_id: &SubnetId) -> String {
        subnet_id.0.clone()
    }

    /// Load a bitmap from the database, or create a fresh one if none exists.
    fn load_bitmap(&self, subnet_id: &SubnetId) -> Result<IpamBitmap> {
        let key = Self::bitmap_key(subnet_id);
        match self.db.get::<IpamBitmap>(IPAM_BITMAPS_TABLE, &key)? {
            Some(bm) => Ok(bm),
            None => Ok(IpamBitmap::new()),
        }
    }

    /// Save a bitmap to the database.
    fn save_bitmap(&self, subnet_id: &SubnetId, bitmap: &IpamBitmap) -> Result<()> {
        let key = Self::bitmap_key(subnet_id);
        self.db.set(IPAM_BITMAPS_TABLE, &key, bitmap)?;
        Ok(())
    }

    /// Allocate the next available IP in the subnet.
    ///
    /// Finds the first free bit in the bitmap, sets it, persists the update, and
    /// returns the corresponding IPv4 address.
    pub fn allocate(&self, subnet: &Subnet) -> Result<Ipv4Addr> {
        let base = parse_subnet_base(subnet)?;
        let mut bitmap = self.load_bitmap(&subnet.id)?;

        let index = bitmap
            .find_first_free()
            .ok_or(OrgError::IpExhausted(subnet.id.to_string()))?;

        bitmap.allocate(index);
        self.save_bitmap(&subnet.id, &bitmap)?;

        Ok(ip_from_index(base, index))
    }

    /// Release an allocated IP back to the pool.
    ///
    /// Clears the bit for the given IP. Returns an error if the IP is reserved
    /// or not currently allocated.
    pub fn release(&self, subnet: &Subnet, ip: Ipv4Addr) -> Result<()> {
        let base = parse_subnet_base(subnet)?;
        let index = index_from_ip(base, ip)?;

        if is_reserved(index) {
            return Err(OrgError::IpReserved(ip.to_string()));
        }

        let mut bitmap = self.load_bitmap(&subnet.id)?;

        if !bitmap.is_allocated(index) {
            return Err(OrgError::IpNotAllocated(ip.to_string()));
        }

        bitmap.release(index);
        self.save_bitmap(&subnet.id, &bitmap)?;
        Ok(())
    }

    /// Check whether a specific IP is currently allocated.
    pub fn is_allocated(&self, subnet: &Subnet, ip: Ipv4Addr) -> Result<bool> {
        let base = parse_subnet_base(subnet)?;
        let index = index_from_ip(base, ip)?;
        let bitmap = self.load_bitmap(&subnet.id)?;
        Ok(bitmap.is_allocated(index))
    }

    /// Count the number of available (unallocated) IPs in the subnet.
    pub fn available_count(&self, subnet: &Subnet) -> Result<u32> {
        let bitmap = self.load_bitmap(&subnet.id)?;
        Ok(bitmap.available_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EnvironmentId, SubnetId, VpcId};

    fn temp_allocator() -> (tempfile::TempDir, IpamAllocator) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();
        (dir, IpamAllocator::new(db))
    }

    fn test_subnet() -> Subnet {
        Subnet {
            id: SubnetId("subnet-frontend".to_string()),
            name: "frontend".to_string(),
            vpc_id: VpcId("vpc-default".to_string()),
            env_id: EnvironmentId("env-prod".to_string()),
            cidr: "10.0.1.0/24".to_string(),
            gateway: "10.0.1.1".to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn allocate_first_ip() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        let ip = alloc.allocate(&subnet).unwrap();
        // .0, .1, .2 are reserved, so first available is .3
        assert_eq!(ip, Ipv4Addr::new(10, 0, 1, 3));
    }

    #[test]
    fn allocate_sequential() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        let ip1 = alloc.allocate(&subnet).unwrap();
        let ip2 = alloc.allocate(&subnet).unwrap();
        let ip3 = alloc.allocate(&subnet).unwrap();

        assert_eq!(ip1, Ipv4Addr::new(10, 0, 1, 3));
        assert_eq!(ip2, Ipv4Addr::new(10, 0, 1, 4));
        assert_eq!(ip3, Ipv4Addr::new(10, 0, 1, 5));
    }

    #[test]
    fn release_and_reallocate() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        let ip1 = alloc.allocate(&subnet).unwrap();
        assert_eq!(ip1, Ipv4Addr::new(10, 0, 1, 3));

        let _ip2 = alloc.allocate(&subnet).unwrap(); // .4

        // Release .3
        alloc.release(&subnet, Ipv4Addr::new(10, 0, 1, 3)).unwrap();

        // Next allocation should reuse .3
        let ip3 = alloc.allocate(&subnet).unwrap();
        assert_eq!(ip3, Ipv4Addr::new(10, 0, 1, 3));
    }

    #[test]
    fn skip_reserved_ips() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        // .0, .1, .2, .255 should always be marked as allocated
        assert!(alloc
            .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 0))
            .unwrap());
        assert!(alloc
            .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 1))
            .unwrap());
        assert!(alloc
            .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 2))
            .unwrap());
        assert!(alloc
            .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 255))
            .unwrap());

        // First allocation must be .3
        let ip = alloc.allocate(&subnet).unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 0, 1, 3));

        // Attempting to release a reserved IP should fail
        let err = alloc
            .release(&subnet, Ipv4Addr::new(10, 0, 1, 0))
            .unwrap_err();
        assert!(matches!(err, OrgError::IpReserved(_)));
    }

    #[test]
    fn bitmap_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-persist.redb");
        let subnet = test_subnet();

        // Allocate some IPs, then drop the allocator
        {
            let db = syfrah_state::LayerDb::open_at(&path).unwrap();
            let alloc = IpamAllocator::new(db);

            let ip1 = alloc.allocate(&subnet).unwrap();
            let ip2 = alloc.allocate(&subnet).unwrap();
            assert_eq!(ip1, Ipv4Addr::new(10, 0, 1, 3));
            assert_eq!(ip2, Ipv4Addr::new(10, 0, 1, 4));
        }

        // Reopen the database and verify allocations persisted
        {
            let db = syfrah_state::LayerDb::open_at(&path).unwrap();
            let alloc = IpamAllocator::new(db);

            assert!(alloc
                .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 3))
                .unwrap());
            assert!(alloc
                .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 4))
                .unwrap());
            assert!(!alloc
                .is_allocated(&subnet, Ipv4Addr::new(10, 0, 1, 5))
                .unwrap());

            // Next allocation should be .5
            let ip3 = alloc.allocate(&subnet).unwrap();
            assert_eq!(ip3, Ipv4Addr::new(10, 0, 1, 5));
        }
    }

    #[test]
    fn ip_exhausted() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        // Allocate all 252 available IPs (.3 through .254)
        for _ in 0..252 {
            alloc.allocate(&subnet).unwrap();
        }

        // Next allocation should fail
        let err = alloc.allocate(&subnet).unwrap_err();
        assert!(matches!(err, OrgError::IpExhausted(_)));
    }

    #[test]
    fn release_unallocated_ip_fails() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        let err = alloc
            .release(&subnet, Ipv4Addr::new(10, 0, 1, 50))
            .unwrap_err();
        assert!(matches!(err, OrgError::IpNotAllocated(_)));
    }

    #[test]
    fn available_count_decreases() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        // Fresh subnet: 252 available (.3-.254)
        assert_eq!(alloc.available_count(&subnet).unwrap(), 252);

        alloc.allocate(&subnet).unwrap();
        assert_eq!(alloc.available_count(&subnet).unwrap(), 251);

        alloc.allocate(&subnet).unwrap();
        assert_eq!(alloc.available_count(&subnet).unwrap(), 250);
    }

    #[test]
    fn ip_already_allocated() {
        let (_dir, alloc) = temp_allocator();
        let subnet = test_subnet();

        let ip = alloc.allocate(&subnet).unwrap();
        assert!(alloc.is_allocated(&subnet, ip).unwrap());
    }
}
