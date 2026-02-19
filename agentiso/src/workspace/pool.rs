use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::info;
use uuid::Uuid;

use crate::config::PoolConfig;

/// A pre-booted VM ready for assignment.
#[derive(Debug)]
#[allow(dead_code)]
pub struct WarmVm {
    pub id: Uuid,
    pub vsock_cid: u32,
    pub zfs_dataset: String,
    pub zvol_path: PathBuf,
    pub qemu_pid: u32,
    pub booted_at: Instant,
    /// Short ID used for TAP device naming and ZFS dataset naming.
    pub short_id: String,
    /// TAP device allocated for this pool VM (needed by QEMU at launch).
    pub tap_device: String,
    /// IP address allocated for this pool VM's TAP.
    pub guest_ip: Ipv4Addr,
    /// Memory allocated to this VM in MB (for budget tracking).
    pub memory_mb: u32,
}

/// Adaptive warm VM pool for sub-second workspace assignment.
pub struct VmPool {
    ready: RwLock<VecDeque<WarmVm>>,
    config: PoolConfig,
}

impl VmPool {
    pub fn new(config: PoolConfig) -> Self {
        Self {
            ready: RwLock::new(VecDeque::new()),
            config,
        }
    }

    /// Check if the pool is enabled.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Try to claim a warm VM from the pool.
    pub async fn claim(&self) -> Option<WarmVm> {
        let mut ready = self.ready.write().await;
        let vm = ready.pop_front();
        if let Some(ref vm) = vm {
            info!(
                pool_id = %vm.id,
                cid = vm.vsock_cid,
                age_ms = vm.booted_at.elapsed().as_millis() as u64,
                remaining = ready.len(),
                "claimed warm VM from pool"
            );
        }
        vm
    }

    /// Add a booted VM to the ready queue.
    pub async fn add_ready(&self, vm: WarmVm) {
        let mut ready = self.ready.write().await;
        info!(
            pool_id = %vm.id,
            cid = vm.vsock_cid,
            pool_size = ready.len() + 1,
            "warm VM added to pool"
        );
        ready.push_back(vm);
    }

    /// Get the current pool size (ready VMs).
    #[allow(dead_code)] // public API used in tests, useful for monitoring
    pub async fn ready_count(&self) -> usize {
        self.ready.read().await.len()
    }

    /// Check if the pool needs more VMs to reach target_free.
    #[allow(dead_code)] // public API used in tests, useful for pool management
    pub async fn needs_replenish(&self) -> bool {
        let count = self.ready.read().await.len();
        count < self.config.target_free
    }

    /// Get the number of VMs to boot to reach target_free.
    pub async fn deficit(&self) -> usize {
        let count = self.ready.read().await.len();
        if count < self.config.target_free {
            (self.config.target_free - count).min(self.config.max_size - count)
        } else {
            0
        }
    }

    /// Drain all VMs from the pool (for shutdown).
    pub async fn drain(&self) -> Vec<WarmVm> {
        let mut ready = self.ready.write().await;
        ready.drain(..).collect()
    }

    /// Total memory in MB consumed by all pool VMs.
    pub async fn total_memory_mb(&self) -> usize {
        let ready = self.ready.read().await;
        ready.iter().map(|vm| vm.memory_mb as usize).sum()
    }

    /// Maximum memory budget in MB for the pool.
    pub fn max_memory_mb(&self) -> usize {
        self.config.max_memory_mb
    }

    /// Pool configuration.
    #[allow(dead_code)]
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }

    /// Target number of free VMs the pool should maintain.
    #[allow(dead_code)] // public API: used by pool management and tests
    pub fn target_free(&self) -> usize {
        self.config.target_free
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PoolConfig {
        PoolConfig {
            enabled: true,
            min_size: 2,
            max_size: 10,
            target_free: 3,
            max_memory_mb: 8192,
        }
    }

    fn make_warm_vm(cid: u32) -> WarmVm {
        let id = Uuid::new_v4();
        let short_id = id.to_string()[..8].to_string();
        WarmVm {
            id,
            vsock_cid: cid,
            zfs_dataset: format!("agentiso/agentiso/pool/warm-{}", Uuid::new_v4()),
            zvol_path: PathBuf::from("/dev/zvol/test"),
            qemu_pid: 1234,
            booted_at: Instant::now(),
            short_id: short_id.clone(),
            tap_device: format!("tap-{}", &short_id),
            guest_ip: Ipv4Addr::new(10, 99, 0, cid as u8),
            memory_mb: 512,
        }
    }

    #[tokio::test]
    async fn test_pool_claim_empty() {
        let pool = VmPool::new(test_config());
        assert!(pool.claim().await.is_none());
    }

    #[tokio::test]
    async fn test_pool_add_and_claim() {
        let pool = VmPool::new(test_config());
        let vm = make_warm_vm(100);
        let expected_cid = vm.vsock_cid;
        pool.add_ready(vm).await;

        assert_eq!(pool.ready_count().await, 1);
        let claimed = pool.claim().await.unwrap();
        assert_eq!(claimed.vsock_cid, expected_cid);
        assert_eq!(pool.ready_count().await, 0);
    }

    #[tokio::test]
    async fn test_pool_fifo_order() {
        let pool = VmPool::new(test_config());
        pool.add_ready(make_warm_vm(100)).await;
        pool.add_ready(make_warm_vm(101)).await;
        pool.add_ready(make_warm_vm(102)).await;

        assert_eq!(pool.claim().await.unwrap().vsock_cid, 100);
        assert_eq!(pool.claim().await.unwrap().vsock_cid, 101);
        assert_eq!(pool.claim().await.unwrap().vsock_cid, 102);
        assert!(pool.claim().await.is_none());
    }

    #[tokio::test]
    async fn test_pool_needs_replenish() {
        let pool = VmPool::new(test_config());
        assert!(pool.needs_replenish().await);
        assert_eq!(pool.deficit().await, 3);

        pool.add_ready(make_warm_vm(100)).await;
        assert!(pool.needs_replenish().await);
        assert_eq!(pool.deficit().await, 2);

        pool.add_ready(make_warm_vm(101)).await;
        pool.add_ready(make_warm_vm(102)).await;
        assert!(!pool.needs_replenish().await);
        assert_eq!(pool.deficit().await, 0);
    }

    #[tokio::test]
    async fn test_pool_drain() {
        let pool = VmPool::new(test_config());
        pool.add_ready(make_warm_vm(100)).await;
        pool.add_ready(make_warm_vm(101)).await;

        let drained = pool.drain().await;
        assert_eq!(drained.len(), 2);
        assert_eq!(pool.ready_count().await, 0);
    }

    #[test]
    fn test_pool_disabled() {
        let config = PoolConfig {
            enabled: false,
            ..PoolConfig::default()
        };
        let pool = VmPool::new(config);
        assert!(!pool.enabled());
    }
}
