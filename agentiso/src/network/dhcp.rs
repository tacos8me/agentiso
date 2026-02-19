use std::collections::BTreeSet;
use std::net::Ipv4Addr;

use anyhow::{bail, Result};
use tracing::{debug, info};

/// Sequential IP allocator for the 10.99.0.0/16 subnet.
///
/// Allocates IPs starting from 10.99.0.2 (10.99.0.1 is the bridge/gateway).
/// Tracks allocated addresses and reclaims them on release.
///
/// This is not DHCP -- the name refers to the IP allocation role. Guest IPs are
/// configured via the guest agent, not via a DHCP server.
pub struct IpAllocator {
    /// Subnet prefix (first two octets), e.g. [10, 99]
    prefix: [u8; 2],
    /// Set of currently allocated IPs (stored as the last two octets combined into u16)
    allocated: BTreeSet<u16>,
    /// Next candidate offset to try (starts at 2, since .0.0 is network and .0.1 is gateway)
    next_candidate: u16,
    /// Gateway offset (reserved, must never be released)
    gateway_offset: u16,
}

#[allow(dead_code)]
impl IpAllocator {
    /// Create a new allocator for the given /16 subnet.
    ///
    /// `gateway` is the bridge IP (e.g. 10.99.0.1), which is pre-reserved.
    pub fn new(gateway: Ipv4Addr) -> Self {
        let octets = gateway.octets();
        let prefix = [octets[0], octets[1]];

        // Pre-reserve the gateway address and network address
        let mut allocated = BTreeSet::new();
        // Reserve .0.0 (network address)
        allocated.insert(0);
        // Reserve the gateway
        let gw_offset = u16::from(octets[2]) << 8 | u16::from(octets[3]);
        allocated.insert(gw_offset);

        Self {
            prefix,
            allocated,
            // Start allocating from .0.2
            next_candidate: 2,
            gateway_offset: gw_offset,
        }
    }

    /// Allocate the next available IP address.
    ///
    /// Returns an error if the pool is exhausted (unlikely with /16).
    pub fn allocate(&mut self) -> Result<Ipv4Addr> {
        // Try from next_candidate, wrapping around the /16 space
        // Skip 0 (network) and 0xFFFF (broadcast-ish)
        let max_offset: u16 = 0xFFFE; // 65534
        let start = self.next_candidate;
        let mut offset = start;

        loop {
            if offset == 0 || offset > max_offset {
                offset = 2; // skip network and gateway area
            }

            if !self.allocated.contains(&offset) {
                self.allocated.insert(offset);
                self.next_candidate = offset.wrapping_add(1);

                let ip = self.offset_to_ip(offset);
                debug!(ip = %ip, offset = offset, "allocated IP");
                return Ok(ip);
            }

            offset = offset.wrapping_add(1);
            if offset == 0 {
                offset = 2;
            }

            // Full loop -- pool exhausted
            if offset == start {
                bail!("IP address pool exhausted (all addresses in {}.{}.0.0/16 allocated)",
                    self.prefix[0], self.prefix[1]);
            }
        }
    }

    /// Release a previously allocated IP back to the pool.
    ///
    /// Reserved addresses (network, gateway, broadcast) cannot be released.
    pub fn release(&mut self, ip: Ipv4Addr) {
        let octets = ip.octets();
        let offset = u16::from(octets[2]) << 8 | u16::from(octets[3]);

        // Guard against releasing reserved addresses
        if offset == 0 || offset == self.gateway_offset || offset == 0xFFFF {
            debug!(ip = %ip, "refusing to release reserved IP");
            return;
        }

        if self.allocated.remove(&offset) {
            // Reset next_candidate to the released offset if it's lower,
            // to try to reuse freed addresses promptly
            if offset < self.next_candidate {
                self.next_candidate = offset;
            }
            info!(ip = %ip, "released IP back to pool");
        } else {
            debug!(ip = %ip, "attempted to release unallocated IP");
        }
    }

    /// Mark an IP as allocated (for restoring state from persistence).
    pub fn mark_allocated(&mut self, ip: Ipv4Addr) -> Result<()> {
        let octets = ip.octets();
        if octets[0] != self.prefix[0] || octets[1] != self.prefix[1] {
            bail!(
                "IP {} is not in subnet {}.{}.0.0/16",
                ip,
                self.prefix[0],
                self.prefix[1]
            );
        }

        let offset = u16::from(octets[2]) << 8 | u16::from(octets[3]);
        self.allocated.insert(offset);

        // Advance next_candidate past any already-allocated block
        if offset >= self.next_candidate {
            self.next_candidate = offset.wrapping_add(1);
        }

        Ok(())
    }

    /// Returns how many IPs are currently allocated (including reserved ones).
    pub fn allocated_count(&self) -> usize {
        self.allocated.len()
    }

    /// Returns the total pool capacity (65534 usable addresses in a /16).
    pub fn capacity(&self) -> usize {
        65534
    }

    /// Convert a /16 offset to an IP address.
    fn offset_to_ip(&self, offset: u16) -> Ipv4Addr {
        let third = (offset >> 8) as u8;
        let fourth = (offset & 0xFF) as u8;
        Ipv4Addr::new(self.prefix[0], self.prefix[1], third, fourth)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_allocation() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));

        let ip1 = alloc.allocate().unwrap();
        assert_eq!(ip1, Ipv4Addr::new(10, 99, 0, 2));

        let ip2 = alloc.allocate().unwrap();
        assert_eq!(ip2, Ipv4Addr::new(10, 99, 0, 3));

        let ip3 = alloc.allocate().unwrap();
        assert_eq!(ip3, Ipv4Addr::new(10, 99, 0, 4));
    }

    #[test]
    fn test_release_and_reuse() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));

        let ip1 = alloc.allocate().unwrap();
        let ip2 = alloc.allocate().unwrap();
        let _ip3 = alloc.allocate().unwrap();

        // Release ip1
        alloc.release(ip1);
        assert_eq!(alloc.allocated_count(), 4); // 2 reserved + ip2 + ip3

        // Next allocation should reuse ip1's offset since it's lower
        let ip4 = alloc.allocate().unwrap();
        assert_eq!(ip4, ip1);

        // Then continue from where we left off
        let ip5 = alloc.allocate().unwrap();
        assert_eq!(ip5, Ipv4Addr::new(10, 99, 0, 5));

        // Release ip2 but next_candidate is already past it
        alloc.release(ip2);
        let ip6 = alloc.allocate().unwrap();
        assert_eq!(ip6, ip2); // should reuse ip2 since it was released and is lower
    }

    #[test]
    fn test_mark_allocated() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));

        alloc.mark_allocated(Ipv4Addr::new(10, 99, 0, 2)).unwrap();
        alloc.mark_allocated(Ipv4Addr::new(10, 99, 0, 3)).unwrap();

        // Should skip already-allocated addresses
        let ip = alloc.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 99, 0, 4));
    }

    #[test]
    fn test_higher_octets() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));

        // Mark a bunch of addresses to push into higher octets
        for i in 2u16..258 {
            let third = (i >> 8) as u8;
            let fourth = (i & 0xFF) as u8;
            alloc
                .mark_allocated(Ipv4Addr::new(10, 99, third, fourth))
                .unwrap();
        }

        let ip = alloc.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 99, 1, 2)); // 258 = 0x0102
    }

    #[test]
    fn test_wrong_subnet_rejected() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        assert!(alloc
            .mark_allocated(Ipv4Addr::new(192, 168, 1, 1))
            .is_err());
    }

    #[test]
    fn test_capacity() {
        let alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        assert_eq!(alloc.capacity(), 65534);
    }

    #[test]
    fn test_initial_allocated_count() {
        let alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        // Network address (0.0) and gateway (0.1) are pre-reserved
        assert_eq!(alloc.allocated_count(), 2);
    }

    #[test]
    fn test_release_unallocated_ip_is_noop() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        let before = alloc.allocated_count();
        // Release an IP that was never allocated
        alloc.release(Ipv4Addr::new(10, 99, 0, 99));
        assert_eq!(alloc.allocated_count(), before);
    }

    #[test]
    fn test_sequential_allocation_many() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        let mut ips = Vec::new();

        for _ in 0..100 {
            let ip = alloc.allocate().unwrap();
            ips.push(ip);
        }

        // All should be unique
        let unique: std::collections::HashSet<_> = ips.iter().collect();
        assert_eq!(unique.len(), 100);

        // First should be 10.99.0.2
        assert_eq!(ips[0], Ipv4Addr::new(10, 99, 0, 2));
        // Last should be 10.99.0.101
        assert_eq!(ips[99], Ipv4Addr::new(10, 99, 0, 101));
    }

    #[test]
    fn test_pool_exhaustion_small() {
        // Simulate exhaustion by marking nearly all addresses
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));

        // Mark offsets 2 through 0xFFFE as allocated (the entire usable range)
        for offset in 2u16..=0xFFFE {
            let third = (offset >> 8) as u8;
            let fourth = (offset & 0xFF) as u8;
            alloc.mark_allocated(Ipv4Addr::new(10, 99, third, fourth)).unwrap();
        }

        // Pool should be exhausted
        let result = alloc.allocate();
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exhausted"), "Expected exhaustion error, got: {}", err_msg);
    }

    #[test]
    fn test_release_gateway_is_refused() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        let before = alloc.allocated_count();
        // Attempting to release the gateway should be a no-op
        alloc.release(Ipv4Addr::new(10, 99, 0, 1));
        assert_eq!(alloc.allocated_count(), before);
        // First allocation should still be .0.2, not the gateway
        let ip = alloc.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(10, 99, 0, 2));
    }

    #[test]
    fn test_release_network_address_is_refused() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        let before = alloc.allocated_count();
        alloc.release(Ipv4Addr::new(10, 99, 0, 0));
        assert_eq!(alloc.allocated_count(), before);
    }

    #[test]
    fn test_different_gateway_subnet() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(172, 16, 0, 1));
        let ip = alloc.allocate().unwrap();
        assert_eq!(ip, Ipv4Addr::new(172, 16, 0, 2));

        // Wrong subnet should be rejected
        assert!(alloc.mark_allocated(Ipv4Addr::new(10, 99, 0, 5)).is_err());
    }

    #[test]
    fn test_mark_allocated_idempotent() {
        let mut alloc = IpAllocator::new(Ipv4Addr::new(10, 99, 0, 1));
        alloc.mark_allocated(Ipv4Addr::new(10, 99, 0, 5)).unwrap();
        let count_before = alloc.allocated_count();
        // Mark same address again
        alloc.mark_allocated(Ipv4Addr::new(10, 99, 0, 5)).unwrap();
        assert_eq!(alloc.allocated_count(), count_before);
    }
}
