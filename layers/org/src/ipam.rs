//! IPAM — IP Address Management with bitmap allocation and MAC derivation.
//!
//! Each subnet gets a 32-byte bitmap (256 bits for a /24). Reserved addresses:
//! - `.0` (network), `.1` (gateway), `.2` (reserved/DNS), `.255` (broadcast)
//!
//! MAC addresses are derived deterministically from IPs:
//! `02:00:{octet1:02x}:{octet2:02x}:{octet3:02x}:{octet4:02x}`
//!
//! The `02` prefix indicates a locally administered unicast MAC.

use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use syfrah_state::LayerDb;

use crate::error::{OrgError, Result};
use crate::types::SubnetId;

const IPAM_BITMAPS_TABLE: &str = "ipam_bitmaps";
const IP_ALLOCATIONS_TABLE: &str = "ip_allocations";

/// Reserved host offsets within a subnet (not allocatable).
const RESERVED_OFFSETS: &[u8] = &[0, 1, 2, 255];

/// State of an IP allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationState {
    /// IP reserved but VM not yet created.
    Reserved,
    /// IP assigned to a running VM.
    Assigned,
    /// IP was allocated but VM creation failed; awaiting reclamation.
    Orphaned,
}

/// A record tracking the full lifecycle of an IP allocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpAllocation {
    pub ip: String,
    pub subnet_id: SubnetId,
    pub vm_id: Option<String>,
    pub mac: String,
    pub state: AllocationState,
    pub allocated_at: u64,
    pub assigned_at: Option<u64>,
}

/// Derive a deterministic MAC address from an IPv4 address.
///
/// Format: `02:00:{o1:02x}:{o2:02x}:{o3:02x}:{o4:02x}`
///
/// The `02` prefix byte marks a locally administered unicast MAC.
///
/// # Examples
///
/// ```
/// use std::net::Ipv4Addr;
/// use syfrah_org::ipam::mac_from_ip;
///
/// let mac = mac_from_ip(Ipv4Addr::new(10, 0, 1, 5));
/// assert_eq!(mac, "02:00:0a:00:01:05");
/// ```
pub fn mac_from_ip(ip: Ipv4Addr) -> String {
    let octets = ip.octets();
    format!(
        "02:00:{:02x}:{:02x}:{:02x}:{:02x}",
        octets[0], octets[1], octets[2], octets[3]
    )
}

/// Bitmap-based IP allocator for a subnet.
///
/// Each /24 subnet uses 32 bytes (256 bits). Bit set = IP in use.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubnetBitmap {
    /// 32-byte bitmap: bit N corresponds to host offset N.
    bits: Vec<u8>,
}

impl SubnetBitmap {
    /// Create a new bitmap for a /24 with reserved addresses pre-set.
    fn new_slash24() -> Self {
        let mut bm = SubnetBitmap {
            bits: vec![0u8; 32],
        };
        for &offset in RESERVED_OFFSETS {
            bm.set(offset as usize);
        }
        bm
    }

    /// Set bit at the given offset.
    fn set(&mut self, offset: usize) {
        if offset < 256 {
            self.bits[offset / 8] |= 1 << (offset % 8);
        }
    }

    /// Clear bit at the given offset.
    fn clear(&mut self, offset: usize) {
        if offset < 256 {
            self.bits[offset / 8] &= !(1 << (offset % 8));
        }
    }

    /// Check if bit at the given offset is set.
    fn is_set(&self, offset: usize) -> bool {
        if offset >= 256 {
            return true;
        }
        (self.bits[offset / 8] & (1 << (offset % 8))) != 0
    }

    /// Find the first free offset (unset bit), skipping reserved.
    fn first_free(&self) -> Option<usize> {
        (3..255).find(|&offset| !self.is_set(offset))
    }
}

/// IPAM allocator backed by redb.
pub struct IpamAllocator {
    db: LayerDb,
}

impl IpamAllocator {
    /// Create a new IPAM allocator.
    pub fn new(db: LayerDb) -> Self {
        Self { db }
    }

    /// Allocate the next available IP from a subnet.
    ///
    /// Returns an `IpAllocation` with state `Reserved` and a deterministically
    /// derived MAC address.
    pub fn allocate(&self, subnet_id: &SubnetId, subnet_cidr: &str) -> Result<IpAllocation> {
        let cidr: Ipv4Net = subnet_cidr
            .parse()
            .map_err(|_| OrgError::InvalidCidr(subnet_cidr.to_string()))?;

        let prefix = cidr.prefix_len();
        if prefix != 24 {
            return Err(OrgError::IpamUnsupportedPrefix(prefix));
        }

        let network = cidr.network();
        let bitmap_key = subnet_id.0.clone();

        // Load or create bitmap.
        let mut bitmap: SubnetBitmap = self
            .db
            .get(IPAM_BITMAPS_TABLE, &bitmap_key)?
            .unwrap_or_else(SubnetBitmap::new_slash24);

        // Find next free offset.
        let offset = bitmap
            .first_free()
            .ok_or(OrgError::IpamExhausted(subnet_id.0.clone()))?;

        // Compute IP from network base + offset.
        let octets = network.octets();
        let ip = Ipv4Addr::new(octets[0], octets[1], octets[2], offset as u8);

        // Derive MAC.
        let mac = mac_from_ip(ip);

        // Mark bit as allocated.
        bitmap.set(offset);

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let allocation = IpAllocation {
            ip: ip.to_string(),
            subnet_id: subnet_id.clone(),
            vm_id: None,
            mac: mac.clone(),
            state: AllocationState::Reserved,
            allocated_at: now,
            assigned_at: None,
        };

        // Persist bitmap and allocation atomically.
        let alloc_key = format!("{}:{}", subnet_id.0, ip);
        self.db.batch(|w| {
            w.set(IPAM_BITMAPS_TABLE, &bitmap_key, &bitmap)?;
            w.set(IP_ALLOCATIONS_TABLE, &alloc_key, &allocation)?;
            Ok(())
        })?;

        Ok(allocation)
    }

    /// Release an IP allocation, freeing it for reuse.
    pub fn release(&self, subnet_id: &SubnetId, ip: Ipv4Addr) -> Result<()> {
        let bitmap_key = subnet_id.0.clone();
        let alloc_key = format!("{}:{}", subnet_id.0, ip);

        let mut bitmap: SubnetBitmap = self
            .db
            .get(IPAM_BITMAPS_TABLE, &bitmap_key)?
            .ok_or_else(|| OrgError::IpamSubnetNotFound(subnet_id.0.clone()))?;

        let offset = ip.octets()[3] as usize;

        // Don't allow releasing reserved addresses.
        if RESERVED_OFFSETS.contains(&(offset as u8)) {
            return Err(OrgError::IpamReservedAddress(ip.to_string()));
        }

        bitmap.clear(offset);

        self.db.batch(|w| {
            w.set(IPAM_BITMAPS_TABLE, &bitmap_key, &bitmap)?;
            w.delete(IP_ALLOCATIONS_TABLE, &alloc_key)?;
            Ok(())
        })?;

        Ok(())
    }

    /// Get an allocation record by subnet and IP.
    pub fn get_allocation(&self, subnet_id: &SubnetId, ip: &str) -> Result<Option<IpAllocation>> {
        let alloc_key = format!("{}:{}", subnet_id.0, ip);
        Ok(self.db.get(IP_ALLOCATIONS_TABLE, &alloc_key)?)
    }

    /// List all allocations for a subnet.
    pub fn list_allocations(&self, subnet_id: &SubnetId) -> Result<Vec<IpAllocation>> {
        let entries: Vec<(String, IpAllocation)> = self.db.list(IP_ALLOCATIONS_TABLE)?;
        let prefix = format!("{}:", subnet_id.0);
        Ok(entries
            .into_iter()
            .filter(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| v)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_allocator() -> (tempfile::TempDir, IpamAllocator) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-test.redb");
        let db = syfrah_state::LayerDb::open_at(&path).unwrap();
        (dir, IpamAllocator::new(db))
    }

    #[test]
    fn mac_from_ip_example() {
        let mac = mac_from_ip(Ipv4Addr::new(10, 0, 1, 5));
        assert_eq!(mac, "02:00:0a:00:01:05");
    }

    #[test]
    fn mac_uniqueness() {
        let ips = [
            Ipv4Addr::new(10, 0, 1, 5),
            Ipv4Addr::new(10, 0, 1, 6),
            Ipv4Addr::new(10, 0, 2, 5),
            Ipv4Addr::new(192, 168, 1, 1),
            Ipv4Addr::new(172, 16, 0, 1),
        ];
        let macs: Vec<String> = ips.iter().map(|ip| mac_from_ip(*ip)).collect();
        // All MACs must be unique.
        let mut unique = macs.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            macs.len(),
            unique.len(),
            "MAC addresses must be unique for different IPs"
        );
    }

    #[test]
    fn mac_format_valid() {
        let test_ips = [
            Ipv4Addr::new(10, 0, 1, 5),
            Ipv4Addr::new(0, 0, 0, 0),
            Ipv4Addr::new(255, 255, 255, 255),
            Ipv4Addr::new(192, 168, 100, 200),
        ];

        let mac_regex = regex::Regex::new(
            r"^[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}:[0-9a-f]{2}$",
        )
        .unwrap();

        for ip in &test_ips {
            let mac = mac_from_ip(*ip);
            assert!(
                mac_regex.is_match(&mac),
                "MAC '{}' for IP {} does not match XX:XX:XX:XX:XX:XX format",
                mac,
                ip
            );
            // First byte must be 02 (locally administered unicast).
            assert!(mac.starts_with("02:00:"), "MAC must start with 02:00:");
        }
    }

    #[test]
    fn allocate_first_ip() {
        let (_dir, allocator) = temp_allocator();
        let subnet_id = SubnetId("subnet-test".to_string());
        let alloc = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        // First allocatable IP is .3 (after .0 network, .1 gateway, .2 reserved).
        assert_eq!(alloc.ip, "10.0.1.3");
        assert_eq!(alloc.mac, "02:00:0a:00:01:03");
        assert_eq!(alloc.state, AllocationState::Reserved);
        assert!(alloc.vm_id.is_none());
    }

    #[test]
    fn allocate_sequential() {
        let (_dir, allocator) = temp_allocator();
        let subnet_id = SubnetId("subnet-seq".to_string());
        let a1 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        let a2 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        let a3 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        assert_eq!(a1.ip, "10.0.1.3");
        assert_eq!(a2.ip, "10.0.1.4");
        assert_eq!(a3.ip, "10.0.1.5");
    }

    #[test]
    fn release_and_reallocate() {
        let (_dir, allocator) = temp_allocator();
        let subnet_id = SubnetId("subnet-rel".to_string());
        let a1 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        assert_eq!(a1.ip, "10.0.1.3");

        let _a2 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();

        // Release .3.
        allocator
            .release(&subnet_id, Ipv4Addr::new(10, 0, 1, 3))
            .unwrap();

        // Next allocation should reuse .3.
        let a3 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        assert_eq!(a3.ip, "10.0.1.3");
    }

    #[test]
    fn skip_reserved_ips() {
        let (_dir, allocator) = temp_allocator();
        let subnet_id = SubnetId("subnet-rsv".to_string());
        // First allocation must skip .0, .1, .2 and start at .3.
        let alloc = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        assert_eq!(alloc.ip, "10.0.1.3");
    }

    #[test]
    fn bitmap_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ipam-persist.redb");
        let subnet_id = SubnetId("subnet-persist".to_string());

        // Allocate in one session.
        {
            let db = syfrah_state::LayerDb::open_at(&path).unwrap();
            let allocator = IpamAllocator::new(db);
            let a1 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
            assert_eq!(a1.ip, "10.0.1.3");
        }

        // Reopen and allocate again; should continue from .4.
        {
            let db = syfrah_state::LayerDb::open_at(&path).unwrap();
            let allocator = IpamAllocator::new(db);
            let a2 = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
            assert_eq!(a2.ip, "10.0.1.4");
        }
    }

    #[test]
    fn allocation_includes_mac() {
        let (_dir, allocator) = temp_allocator();
        let subnet_id = SubnetId("subnet-mac".to_string());
        let alloc = allocator.allocate(&subnet_id, "10.0.1.0/24").unwrap();
        assert_eq!(
            alloc.mac,
            mac_from_ip(alloc.ip.parse::<Ipv4Addr>().unwrap())
        );
    }
}
