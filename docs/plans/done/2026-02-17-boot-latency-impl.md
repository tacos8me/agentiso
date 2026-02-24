# Boot Latency Optimization Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Sub-second workspace assignment from warm VM pool; ~1-2s cold boot via custom initrd + init bypass.

**Architecture:** Four optimization layers — (1) custom minimal initrd replacing 70MB host initrd, (2) init bypass shell shim replacing OpenRC, (3) guest agent fixes (vsock retry, batched ConfigureWorkspace), (4) adaptive warm VM pool with pre-booted VMs. Host-side parallelism throughout.

**Tech Stack:** Rust, tokio, QEMU microvm, ZFS zvols, vsock, nftables, busybox

**Design doc:** `docs/plans/2026-02-17-boot-latency-design.md`

**Agent scopes (per AGENTS.md):**
- `guest-agent`: Tasks 1-2 (protocol + guest agent)
- `storage-net`: Tasks 3-4 (ZFS pool/ + nftables skip)
- `vm-engine`: Tasks 5-6 (microvm init_mode + vsock configure_workspace)
- `workspace-core`: Tasks 7-10 (config, parallelism, pool manager, integration)
- Build scripts: Task 11 (any agent or manual)

---

### Task 1: Add ConfigureWorkspace protocol type

**Agent:** `guest-agent`
**Files:**
- Modify: `agentiso/src/guest/protocol.rs:17-47` (GuestRequest enum)
- Test: `agentiso/src/guest/protocol.rs` (existing test section)

**Step 1: Add WorkspaceConfig struct and ConfigureWorkspace variant**

In `agentiso/src/guest/protocol.rs`, add the struct after `SetHostnameRequest` (after line 113):

```rust
/// Combined workspace configuration (network + hostname) in a single vsock RTT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    pub ip_address: String,
    pub gateway: String,
    #[serde(default = "default_dns")]
    pub dns: Vec<String>,
    pub hostname: String,
}
```

Add the variant to the `GuestRequest` enum (after `SetHostname` variant, before `Shutdown`):

```rust
    /// Configure workspace in one shot (network + hostname).
    ConfigureWorkspace(WorkspaceConfig),
```

**Step 2: Write the failing test**

Add to the tests module in the same file:

```rust
    #[test]
    fn test_request_configure_workspace_roundtrip() {
        let req = GuestRequest::ConfigureWorkspace(WorkspaceConfig {
            ip_address: "10.99.0.5/16".into(),
            gateway: "10.99.0.1".into(),
            dns: vec!["1.1.1.1".into()],
            hostname: "ws-abc12345".into(),
        });
        let rt = roundtrip_request(&req);
        if let GuestRequest::ConfigureWorkspace(cfg) = rt {
            assert_eq!(cfg.ip_address, "10.99.0.5/16");
            assert_eq!(cfg.gateway, "10.99.0.1");
            assert_eq!(cfg.hostname, "ws-abc12345");
        } else {
            panic!("expected ConfigureWorkspace variant");
        }
    }
```

**Step 3: Run tests**

Run: `cargo test -p agentiso --lib guest::protocol`
Expected: PASS (including new test)

**Step 4: Commit**

```bash
git add agentiso/src/guest/protocol.rs
git commit -m "feat: add ConfigureWorkspace protocol type for batched vsock config"
```

---

### Task 2: Guest agent — vsock retry loop + ConfigureWorkspace handler

**Agent:** `guest-agent`
**Depends on:** Task 1 (protocol type definition — but guest-agent has its own copy)
**Files:**
- Modify: `guest-agent/src/main.rs:25-37` (GuestRequest enum, mirrored types)
- Modify: `guest-agent/src/main.rs:541-561` (handle_request dispatch)
- Modify: `guest-agent/src/main.rs:884-916` (listen function)

**Step 1: Add mirrored types to guest-agent**

In `guest-agent/src/main.rs`, add `WorkspaceConfig` struct after `SetHostnameRequest` (after line 101):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkspaceConfig {
    ip_address: String,
    gateway: String,
    #[serde(default = "default_dns")]
    dns: Vec<String>,
    hostname: String,
}
```

Add variant to the guest-agent's `GuestRequest` enum (after `SetHostname`, before `Shutdown`):

```rust
    ConfigureWorkspace(WorkspaceConfig),
```

**Step 2: Add ConfigureWorkspace handler**

Add this function after `handle_set_hostname` (after line 539):

```rust
async fn handle_configure_workspace(cfg: WorkspaceConfig) -> GuestResponse {
    // Configure network (same logic as handle_configure_network)
    let net_cfg = NetworkConfig {
        ip_address: cfg.ip_address,
        gateway: cfg.gateway,
        dns: cfg.dns,
    };
    let net_result = handle_configure_network(net_cfg).await;
    if matches!(net_result, GuestResponse::Error(_)) {
        return net_result;
    }

    // Set hostname
    let hostname_result = handle_set_hostname(SetHostnameRequest {
        hostname: cfg.hostname,
    }).await;
    if matches!(hostname_result, GuestResponse::Error(_)) {
        return hostname_result;
    }

    GuestResponse::Ok
}
```

**Step 3: Add dispatch in handle_request**

In `handle_request` (line 541-561), add the new arm before `Shutdown`:

```rust
        GuestRequest::ConfigureWorkspace(cfg) => handle_configure_workspace(cfg).await,
```

**Step 4: Replace 1s sleep with tight retry loop in `listen()`**

Replace lines 896-915 (the module load + sleep + retry section) with:

```rust
    // Load vsock modules ourselves and retry with tight polling
    load_vsock_modules();
    for attempt in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        match VsockListener::bind(port) {
            Ok(listener) => {
                info!(port, attempt, "listening on vsock (after module load)");
                return Ok(Listener::Vsock(listener));
            }
            Err(_) if attempt < 19 => continue,
            Err(e) => {
                warn!(error = %e, port, "vsock still unavailable after module load, falling back to TCP");
                let addr = format!("0.0.0.0:{port}");
                let listener = TcpListener::bind(&addr)
                    .await
                    .with_context(|| format!("failed to bind TCP fallback on {addr}"))?;
                info!(addr = %addr, "listening on TCP (fallback)");
                return Ok(Listener::Tcp(listener));
            }
        }
    }
    unreachable!()
```

**Step 5: Build guest agent to verify compilation**

Run: `cargo build -p agentiso-guest`
Expected: compiles with no errors

**Step 6: Commit**

```bash
git add guest-agent/src/main.rs
git commit -m "feat: guest agent ConfigureWorkspace handler + vsock retry loop (50ms intervals)"
```

---

### Task 3: ZFS pool/ dataset helpers

**Agent:** `storage-net`
**Files:**
- Modify: `agentiso/src/storage/zfs.rs:39-312` (Zfs impl)
- Modify: `agentiso/src/storage/mod.rs` (StorageManager)

**Step 1: Add pool_dataset() helper to Zfs**

In `agentiso/src/storage/zfs.rs`, add after `fork_dataset()` (after line 58):

```rust
    /// Full dataset path for a warm pool zvol.
    pub fn pool_dataset(&self, pool_id: &str) -> String {
        format!("{}/pool/warm-{}", self.pool_root, pool_id)
    }

    /// Path to the zvol block device for a warm pool VM.
    pub fn pool_zvol_path(&self, pool_id: &str) -> PathBuf {
        PathBuf::from(format!(
            "/dev/zvol/{}/pool/warm-{}",
            self.pool_root, pool_id
        ))
    }
```

**Step 2: Add "pool" to ensure_pool_layout()**

In `ensure_pool_layout()` (line 301), change the list:

```rust
        for sub in ["base", "workspaces", "forks", "pool"] {
```

**Step 3: Add clone_for_pool() method**

Add after `clone_from_base()`:

```rust
    /// Clone the base image snapshot for a warm pool VM.
    ///
    /// Runs: `zfs clone {pool}/base/{base_image}@{snapshot} {pool}/pool/warm-{id}`
    #[instrument(skip(self))]
    pub async fn clone_for_pool(
        &self,
        base_image: &str,
        base_snapshot: &str,
        pool_id: &str,
    ) -> Result<String> {
        let source = format!("{}/base/{}@{}", self.pool_root, base_image, base_snapshot);
        let target = self.pool_dataset(pool_id);

        debug!(source = %source, target = %target, "cloning base image for warm pool");

        run_zfs(&["clone", &source, &target])
            .await
            .with_context(|| format!("failed to clone {} -> {}", source, target))?;

        Ok(target)
    }
```

**Step 4: Add pool helpers to StorageManager**

In `agentiso/src/storage/mod.rs`, add to the `StorageManager` impl:

```rust
    /// Create storage for a warm pool VM by cloning the base image.
    #[instrument(skip(self))]
    pub async fn create_pool_vm(
        &self,
        base_image: &str,
        base_snapshot: &str,
        pool_id: &str,
    ) -> Result<WorkspaceStorage> {
        let dataset = self
            .zfs
            .clone_for_pool(base_image, base_snapshot, pool_id)
            .await
            .context("failed to clone base image for pool VM")?;

        let zvol_path = self.zfs.pool_zvol_path(pool_id);

        info!(
            dataset = %dataset,
            zvol = %zvol_path.display(),
            "pool VM storage created"
        );

        Ok(WorkspaceStorage { dataset, zvol_path })
    }

    /// Destroy storage for a pool VM or workspace by dataset path.
    #[instrument(skip(self))]
    pub async fn destroy_dataset(&self, dataset: &str) -> Result<()> {
        self.zfs.destroy(dataset).await?;
        info!(dataset = %dataset, "dataset destroyed");
        Ok(())
    }
```

**Step 5: Write tests**

Add to `agentiso/src/storage/zfs.rs` tests:

```rust
    #[test]
    fn test_pool_dataset_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.pool_dataset("abc12345"),
            "tank/agentiso/pool/warm-abc12345"
        );
    }

    #[test]
    fn test_pool_zvol_path() {
        let zfs = Zfs::new("tank/agentiso".to_string());
        assert_eq!(
            zfs.pool_zvol_path("abc12345"),
            PathBuf::from("/dev/zvol/tank/agentiso/pool/warm-abc12345")
        );
    }
```

**Step 6: Run tests and commit**

Run: `cargo test -p agentiso --lib storage`
Expected: PASS

```bash
git add agentiso/src/storage/zfs.rs agentiso/src/storage/mod.rs
git commit -m "feat: add ZFS pool/ dataset helpers for warm VM pool"
```

---

### Task 4: Skip nftables remove on new workspaces

**Agent:** `storage-net`
**Files:**
- Modify: `agentiso/src/network/nftables.rs:132-195` (apply_workspace_rules)

**Step 1: Add `is_new` parameter to apply_workspace_rules**

Change the method signature at line 133:

```rust
    pub async fn apply_workspace_rules(
        &self,
        workspace_id: &str,
        guest_ip: Ipv4Addr,
        policy: &NetworkPolicy,
        is_new: bool,
    ) -> Result<()> {
        // Only remove existing rules if this is an existing workspace
        if !is_new {
            self.remove_workspace_rules(workspace_id).await?;
        }
```

**Step 2: Update callers**

Search for all callers of `apply_workspace_rules` (in `network/mod.rs` or wherever the NetworkManager calls it). Update each call to pass the appropriate `is_new` flag.

In `agentiso/src/network/mod.rs`, find `setup_workspace` (which creates new workspaces — pass `true`) and `update_policy`/`ensure_workspace_network` (which update existing — pass `false`).

**Step 3: Update generate_workspace_rules test helper** (if needed)

No change needed — the test helper generates rule strings, not the apply logic.

**Step 4: Run tests and commit**

Run: `cargo test -p agentiso --lib network`
Expected: PASS

```bash
git add agentiso/src/network/nftables.rs agentiso/src/network/mod.rs
git commit -m "perf: skip nftables remove_workspace_rules for new workspaces (saves 15-40ms)"
```

---

### Task 5: VM microvm — init_mode in kernel cmdline

**Agent:** `vm-engine`
**Files:**
- Modify: `agentiso/src/vm/microvm.rs:1-130` (VmConfig)
- Modify: `agentiso/src/vm/mod.rs:96-124` (VmManager::launch)

**Step 1: Add init_mode field to microvm::VmConfig**

In `agentiso/src/vm/microvm.rs`, add to `VmConfig` struct (after `kernel_cmdline` field, line 16):

```rust
    /// Init mode: "fast" appends init=/sbin/init-fast, "openrc" uses default init.
    pub init_mode: String,
```

**Step 2: Update build_command() to use init_mode**

In `build_command()`, change the kernel cmdline handling (around line 61-62):

```rust
        // Kernel command line
        args.push("-append".into());
        let mut cmdline = self.kernel_cmdline.clone();
        if self.init_mode == "fast" {
            cmdline.push_str(" init=/sbin/init-fast");
        }
        args.push(cmdline);
```

**Step 3: Update Default impl**

In the `Default` impl (line 116-130), add:

```rust
            init_mode: "openrc".into(),
```

**Step 4: Update VmManager::launch() to pass init_mode**

In `agentiso/src/vm/mod.rs`, add `init_mode` parameter to `VmManagerConfig`:

```rust
    /// Init mode: "fast" or "openrc".
    pub init_mode: String,
```

With default:
```rust
            init_mode: "openrc".into(),
```

In `launch()` (line 113-124), set:
```rust
            init_mode: self.config.init_mode.clone(),
```

Also add `initrd_fast_path` to VmManagerConfig for the fast initrd:
```rust
    /// Optional path to the fast initrd (used when init_mode = "fast").
    pub initrd_fast_path: Option<PathBuf>,
```

In `launch()`, conditionally override the initrd:
```rust
            initrd_path: if self.config.init_mode == "fast" {
                self.config.initrd_fast_path.clone().or(self.config.initrd_path.clone())
            } else {
                self.config.initrd_path.clone()
            },
```

**Step 5: Write tests**

Add to microvm.rs tests:

```rust
    #[test]
    fn test_build_command_fast_init_mode() {
        let config = VmConfig {
            init_mode: "fast".into(),
            ..VmConfig::default()
        };
        let cmd = config.build_command();
        let append_idx = cmd.args.iter().position(|a| a == "-append").unwrap();
        let cmdline = &cmd.args[append_idx + 1];
        assert!(cmdline.contains("init=/sbin/init-fast"));
    }

    #[test]
    fn test_build_command_openrc_init_mode() {
        let config = VmConfig {
            init_mode: "openrc".into(),
            ..VmConfig::default()
        };
        let cmd = config.build_command();
        let append_idx = cmd.args.iter().position(|a| a == "-append").unwrap();
        let cmdline = &cmd.args[append_idx + 1];
        assert!(!cmdline.contains("init=/sbin/init-fast"));
    }
```

**Step 6: Run tests and commit**

Run: `cargo test -p agentiso --lib vm::microvm`
Expected: PASS

```bash
git add agentiso/src/vm/microvm.rs agentiso/src/vm/mod.rs
git commit -m "feat: add init_mode support to VmConfig for fast boot path"
```

---

### Task 6: VsockClient — configure_workspace method

**Agent:** `vm-engine`
**Depends on:** Task 1 (protocol type)
**Files:**
- Modify: `agentiso/src/vm/vsock.rs:364-411` (after configure_network)

**Step 1: Add configure_workspace method**

Add after `set_hostname()` in `VsockClient`:

```rust
    /// Configure workspace in one shot (network + hostname, single vsock RTT).
    pub async fn configure_workspace(
        &mut self,
        ip_address: &str,
        gateway: &str,
        dns: Vec<String>,
        hostname: &str,
    ) -> Result<()> {
        let req = GuestRequest::ConfigureWorkspace(protocol::WorkspaceConfig {
            ip_address: ip_address.to_string(),
            gateway: gateway.to_string(),
            dns,
            hostname: hostname.to_string(),
        });

        let resp = self
            .request_with_timeout(&req, Duration::from_secs(10))
            .await?;

        Self::unwrap_response(resp, "configure_workspace")?;
        Ok(())
    }
```

**Step 2: Run tests and commit**

Run: `cargo test -p agentiso --lib vm::vsock`
Expected: PASS

```bash
git add agentiso/src/vm/vsock.rs
git commit -m "feat: add VsockClient::configure_workspace for batched network+hostname config"
```

---

### Task 7: Config — PoolConfig + init_mode

**Agent:** `workspace-core`
**Files:**
- Modify: `agentiso/src/config.rs`

**Step 1: Add PoolConfig struct**

Add after `DefaultResourceLimits` (after line 238):

```rust
/// Warm VM pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PoolConfig {
    /// Enable the warm VM pool.
    pub enabled: bool,
    /// Minimum VMs to keep in the pool.
    pub min_size: usize,
    /// Maximum VMs in the pool.
    pub max_size: usize,
    /// Target number of free (ready) VMs.
    pub target_free: usize,
    /// Total memory budget in MB for pool VMs.
    pub max_memory_mb: usize,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_size: 2,
            max_size: 10,
            target_free: 3,
            max_memory_mb: 8192,
        }
    }
}
```

**Step 2: Add init_mode to VmConfig**

Add to the `VmConfig` struct (after `boot_timeout_secs`):

```rust
    /// Init mode: "fast" for init-fast shim, "openrc" for standard Alpine init.
    pub init_mode: String,
    /// Optional path to the fast initrd image (used when init_mode = "fast").
    pub initrd_fast_path: Option<PathBuf>,
```

Add defaults:
```rust
            init_mode: "openrc".into(),
            initrd_fast_path: None,
```

**Step 3: Add PoolConfig to top-level Config**

Add `pub pool: PoolConfig` to the `Config` struct and `pool: PoolConfig::default()` to its `Default` impl.

**Step 4: Add pool validation**

In `validate()`, add:

```rust
        if self.pool.enabled {
            anyhow::ensure!(
                self.pool.min_size <= self.pool.max_size,
                "pool.min_size must be <= pool.max_size"
            );
            anyhow::ensure!(
                self.pool.target_free <= self.pool.max_size,
                "pool.target_free must be <= pool.max_size"
            );
        }
```

**Step 5: Write tests**

```rust
    #[test]
    fn config_pool_defaults() {
        let config = Config::default();
        assert!(!config.pool.enabled);
        assert_eq!(config.pool.min_size, 2);
        assert_eq!(config.pool.max_size, 10);
        assert_eq!(config.pool.target_free, 3);
        assert_eq!(config.pool.max_memory_mb, 8192);
    }

    #[test]
    fn config_pool_validation_min_exceeds_max() {
        let mut config = Config::default();
        config.pool.enabled = true;
        config.pool.min_size = 15;
        config.pool.max_size = 10;
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_vm_init_mode_default() {
        let config = Config::default();
        assert_eq!(config.vm.init_mode, "openrc");
        assert!(config.vm.initrd_fast_path.is_none());
    }
```

**Step 6: Run tests and commit**

Run: `cargo test -p agentiso --lib config`
Expected: PASS

```bash
git add agentiso/src/config.rs
git commit -m "feat: add PoolConfig and init_mode to config"
```

---

### Task 8: Host-side parallelism — tokio::join! for ZFS + TAP

**Agent:** `workspace-core`
**Depends on:** Tasks 4, 6, 7
**Files:**
- Modify: `agentiso/src/workspace/mod.rs:298-452` (create method)

**Step 1: Parallelize ZFS clone + TAP/nftables in create()**

Replace the sequential steps 1+2 (lines 352-375) with parallel execution:

```rust
        // 1+2. Clone storage + set up networking in parallel
        let default_policy = NetworkPolicy {
            allow_internet: self.config.network.default_allow_internet,
            allow_inter_vm: self.config.network.default_allow_inter_vm,
            allowed_ports: Vec::new(),
        };

        let (storage_result, network_result) = tokio::join!(
            self.storage.create_workspace(&base_image, &self.config.storage.base_snapshot, &short_id),
            async { self.network.write().await.setup_workspace(&short_id, &default_policy).await }
        );

        let ws_storage = match storage_result {
            Ok(s) => s,
            Err(e) => {
                // If network succeeded, clean it up
                if let Ok(ref net) = network_result {
                    let _ = self.network.write().await.cleanup_workspace(&short_id, net.guest_ip).await;
                }
                return Err(e.context("failed to create workspace storage"));
            }
        };

        let net_setup = match network_result {
            Ok(n) => n,
            Err(e) => {
                // Rollback storage
                if let Err(e2) = self.storage.destroy_workspace(&short_id).await {
                    error!(error = %e2, "failed to rollback storage after network setup failure");
                }
                return Err(e.context("failed to set up workspace networking"));
            }
        };
```

**Step 2: Use batched configure_workspace instead of two vsock RTTs**

Replace the two separate vsock calls (lines 404-419) with one batched call:

```rust
        // 5. Configure guest workspace via guest agent (single vsock RTT)
        {
            let mut vm = self.vm.write().await;
            let vsock = vm.vsock_client(&id)?;
            if let Err(e) = vsock.configure_workspace(
                &format!("{}/16", net_setup.guest_ip),
                &net_setup.gateway_ip.to_string(),
                vec!["1.1.1.1".to_string()],
                &name,
            ).await {
                warn!(workspace_id = %id, error = %e, "failed to configure guest workspace");
            }
        }
```

**Step 3: Also update fork() with same parallelism pattern**

Apply the same `tokio::join!` pattern to the `fork()` method (lines 1044-1064) and use `configure_workspace()` instead of separate `configure_network()` + `set_hostname()` calls.

**Step 4: Run tests and commit**

Run: `cargo test -p agentiso`
Expected: PASS (all 172+ tests)

```bash
git add agentiso/src/workspace/mod.rs
git commit -m "perf: parallelize ZFS+TAP setup and batch vsock config in workspace create"
```

---

### Task 9: Warm VM Pool manager

**Agent:** `workspace-core`
**Depends on:** Tasks 3, 5, 7
**Files:**
- Create: `agentiso/src/workspace/pool.rs`
- Modify: `agentiso/src/workspace/mod.rs:1` (add `pub mod pool;`)

**Step 1: Create pool.rs with VmPool struct**

Create `agentiso/src/workspace/pool.rs`:

```rust
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::config::{Config, PoolConfig};
use crate::storage::StorageManager;
use crate::vm::{VmManager, VmManagerConfig};

/// A pre-booted VM ready for assignment.
#[derive(Debug)]
pub struct WarmVm {
    pub id: Uuid,
    pub vsock_cid: u32,
    pub zfs_dataset: String,
    pub zvol_path: PathBuf,
    pub qemu_pid: u32,
    pub booted_at: Instant,
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
    ///
    /// Returns `Some(WarmVm)` if a pre-booted VM is available, `None` if pool is empty.
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
    pub async fn ready_count(&self) -> usize {
        self.ready.read().await.len()
    }

    /// Check if the pool needs more VMs to reach target_free.
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

    /// Pool configuration.
    pub fn config(&self) -> &PoolConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PoolConfig;

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
        WarmVm {
            id: Uuid::new_v4(),
            vsock_cid: cid,
            zfs_dataset: format!("agentiso/agentiso/pool/warm-{}", Uuid::new_v4()),
            zvol_path: PathBuf::from("/dev/zvol/test"),
            qemu_pid: 1234,
            booted_at: Instant::now(),
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
```

**Step 2: Add module declaration**

In `agentiso/src/workspace/mod.rs`, add at line 1:

```rust
pub mod pool;
```

**Step 3: Run tests and commit**

Run: `cargo test -p agentiso --lib workspace::pool`
Expected: PASS

```bash
git add agentiso/src/workspace/pool.rs agentiso/src/workspace/mod.rs
git commit -m "feat: add VmPool manager for adaptive warm VM pool"
```

---

### Task 10: Workspace warm pool integration

**Agent:** `workspace-core`
**Depends on:** Tasks 8, 9
**Files:**
- Modify: `agentiso/src/workspace/mod.rs` (WorkspaceManager)

**Step 1: Add VmPool field to WorkspaceManager**

Add to the struct:
```rust
    pool: pool::VmPool,
```

Update `new()` to accept and store it:
```rust
    pub fn new(
        config: Config,
        vm: VmManager,
        storage: StorageManager,
        network: NetworkManager,
        pool: pool::VmPool,
    ) -> Self {
```

**Step 2: Add warm pool claim fast path in create()**

At the start of `create()`, after resource limit checks and before the storage/network setup, add:

```rust
        // Fast path: try to claim a warm VM from the pool
        if self.pool.enabled() {
            if let Some(warm_vm) = self.pool.claim().await {
                return self.assign_warm_vm(warm_vm, id, name, &default_policy).await;
            }
            debug!("warm pool empty, falling back to cold create");
        }
```

**Step 3: Add assign_warm_vm method**

Add a new private method:

```rust
    /// Assign a warm VM from the pool to a workspace.
    ///
    /// The warm VM already has a booted guest agent with vsock.
    /// We just need to: set up TAP/nftables, configure workspace via vsock.
    async fn assign_warm_vm(
        &self,
        warm_vm: pool::WarmVm,
        id: Uuid,
        name: String,
        policy: &NetworkPolicy,
    ) -> Result<Workspace> {
        let short_id = id.to_string()[..8].to_string();

        // Set up networking (TAP + nftables) — this is the main cost (~100ms)
        let net_setup = match self.network.write().await.setup_workspace(&short_id, policy).await {
            Ok(setup) => setup,
            Err(e) => {
                // Return warm VM to pool on failure
                self.pool.add_ready(warm_vm).await;
                return Err(e.context("failed to set up networking for warm VM assignment"));
            }
        };

        // Configure workspace via vsock (single RTT, ~50ms)
        {
            let mut vm = self.vm.write().await;
            if let Ok(vsock) = vm.vsock_client_by_cid(warm_vm.vsock_cid) {
                if let Err(e) = vsock.configure_workspace(
                    &format!("{}/16", net_setup.guest_ip),
                    &net_setup.gateway_ip.to_string(),
                    vec!["1.1.1.1".to_string()],
                    &name,
                ).await {
                    warn!(workspace_id = %id, error = %e, "failed to configure warm VM workspace");
                }
            }
        }

        let workspace = Workspace {
            id,
            name,
            state: WorkspaceState::Running,
            base_image: self.config.storage.base_image.clone(),
            zfs_dataset: warm_vm.zfs_dataset,
            qemu_pid: Some(warm_vm.qemu_pid),
            qmp_socket: self.config.vm.run_dir.join(&short_id).join("qmp.sock"),
            vsock_cid: warm_vm.vsock_cid,
            tap_device: net_setup.tap_device,
            network: WorkspaceNetwork {
                ip: net_setup.guest_ip,
                allow_internet: policy.allow_internet,
                allow_inter_vm: policy.allow_inter_vm,
                allowed_ports: policy.allowed_ports.clone(),
                port_forwards: Vec::new(),
            },
            snapshots: SnapshotTree::new(),
            created_at: Utc::now(),
            resources: ResourceLimits {
                vcpus: self.config.resources.default_vcpus,
                memory_mb: self.config.resources.default_memory_mb,
                disk_gb: self.config.resources.default_disk_gb,
            },
        };

        self.workspaces.write().await.insert(id, workspace.clone());
        self.save_state().await.ok();

        info!(
            workspace_id = %id,
            warm_vm_id = %warm_vm.id,
            "workspace assigned from warm pool"
        );
        Ok(workspace)
    }
```

**Note:** This requires adding a `vsock_client_by_cid` method to `VmManager` that looks up a VM handle by CID rather than workspace ID. This is needed because the warm VM was pre-booted with a CID but not yet assigned to a workspace UUID. Add this to `vm/mod.rs`:

```rust
    /// Get a vsock client by CID (for warm pool VMs not yet assigned to a workspace).
    pub fn vsock_client_by_cid(&mut self, cid: u32) -> Result<&mut VsockClient> {
        for handle in self.vms.values_mut() {
            if handle.config.vsock_cid == cid {
                return Ok(&mut handle.vsock);
            }
        }
        bail!("no VM with vsock CID {}", cid)
    }
```

**Step 4: Add pool accessor and shutdown integration**

In `shutdown_all()`, add pool drain:

```rust
    pub async fn shutdown_all(&self) {
        info!("shutting down all workspaces");

        // Drain warm pool VMs
        let pool_vms = self.pool.drain().await;
        for warm_vm in pool_vms {
            info!(pool_id = %warm_vm.id, "destroying warm pool VM");
            // Kill QEMU process
            unsafe { libc::kill(warm_vm.qemu_pid as i32, libc::SIGKILL); }
            // Destroy ZFS dataset
            if let Err(e) = self.storage.destroy_dataset(&warm_vm.zfs_dataset).await {
                warn!(error = %e, "failed to destroy warm pool VM storage");
            }
        }

        self.vm.write().await.shutdown_all().await;
        // ... rest unchanged
    }
```

**Step 5: Update all callers of WorkspaceManager::new()**

In `agentiso/src/main.rs` and tests, add the pool parameter.

**Step 6: Run tests and commit**

Run: `cargo test -p agentiso`
Expected: PASS

```bash
git add agentiso/src/workspace/mod.rs agentiso/src/vm/mod.rs agentiso/src/main.rs
git commit -m "feat: integrate warm VM pool into workspace create fast path"
```

---

### Task 11: Build scripts — custom initrd + init-fast shim

**Agent:** Any (or manual)
**Files:**
- Create: `images/kernel/build-initrd.sh`
- Modify: `scripts/setup-e2e.sh`

**Step 1: Create build-initrd.sh**

Create `images/kernel/build-initrd.sh`:

```bash
#!/usr/bin/env bash
#
# Build a minimal initrd (~1MB) for fast VM boot.
# Contains only busybox + vsock kernel modules.
#
# Output: /var/lib/agentiso/initrd-fast.img
#
set -euo pipefail

OUTPUT="${1:-/var/lib/agentiso/initrd-fast.img}"
KVER="$(uname -r)"
KMOD_SRC="/lib/modules/$KVER"
WORKDIR=$(mktemp -d)

echo "Building minimal initrd for kernel $KVER..."

# Create directory structure
mkdir -p "$WORKDIR"/{bin,lib/modules,dev,proc,sys,mnt/root}

# Copy busybox (statically linked)
BUSYBOX=$(which busybox)
if [[ ! -f "$BUSYBOX" ]]; then
    echo "ERROR: busybox not found"
    exit 1
fi
cp "$BUSYBOX" "$WORKDIR/bin/busybox"

# Create symlinks for required applets
for cmd in sh mount insmod switch_root; do
    ln -sf busybox "$WORKDIR/bin/$cmd"
done

# Copy and decompress vsock modules
for mod_name in vsock vmw_vsock_virtio_transport_common vmw_vsock_virtio_transport; do
    src=$(find "$KMOD_SRC" -name "${mod_name}.ko*" 2>/dev/null | head -1)
    if [[ -z "$src" ]]; then
        echo "WARNING: module $mod_name not found"
        continue
    fi
    if [[ "$src" == *.zst ]]; then
        zstd -dq "$src" -o "$WORKDIR/lib/modules/${mod_name}.ko"
    elif [[ "$src" == *.xz ]]; then
        xz -dkc "$src" > "$WORKDIR/lib/modules/${mod_name}.ko"
    elif [[ "$src" == *.gz ]]; then
        gzip -dkc "$src" > "$WORKDIR/lib/modules/${mod_name}.ko"
    else
        cp "$src" "$WORKDIR/lib/modules/"
    fi
done

# Create init script
cat > "$WORKDIR/init" << 'INITEOF'
#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
# vsock modules
/bin/busybox insmod /lib/modules/vsock.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport_common.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport.ko 2>/dev/null
# Mount root and switch
/bin/busybox mount -t ext4 /dev/vda /mnt/root
exec /bin/busybox switch_root /mnt/root /sbin/init-fast
INITEOF
chmod 755 "$WORKDIR/init"

# Create cpio archive compressed with gzip
echo "Creating compressed cpio archive..."
(cd "$WORKDIR" && find . | cpio -o -H newc --quiet | gzip -9) > "$OUTPUT"

# Cleanup
rm -rf "$WORKDIR"

SIZE=$(ls -lh "$OUTPUT" | awk '{print $5}')
echo "Built: $OUTPUT ($SIZE)"
```

Make executable: `chmod +x images/kernel/build-initrd.sh`

**Step 2: Create init-fast shim**

Add to `scripts/setup-e2e.sh` (before the unmount step), install the init-fast shim into the Alpine rootfs:

```bash
echo "Installing init-fast shim..."
cat > "$MOUNT_POINT/sbin/init-fast" << 'INITFAST'
#!/bin/sh
# Minimal init shim — bypasses OpenRC for fast boot.
# Mount essential filesystems if not already from initrd.
mountpoint -q /proc || mount -t proc proc /proc
mountpoint -q /sys  || mount -t sysfs sysfs /sys
mountpoint -q /dev  || mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp
mount -t tmpfs tmpfs /run
# Ensure vsock modules loaded (may already be from initrd)
KDIR="/lib/modules/$(uname -r)/kernel/net/vmw_vsock"
insmod "$KDIR/vsock.ko" 2>/dev/null
insmod "$KDIR/vmw_vsock_virtio_transport_common.ko" 2>/dev/null
insmod "$KDIR/vmw_vsock_virtio_transport.ko" 2>/dev/null
# Reap zombies in background
trap 'wait' CHLD
# Exec guest agent (becomes main process under PID 1 shell)
exec /usr/local/bin/agentiso-guest
INITFAST
chmod 755 "$MOUNT_POINT/sbin/init-fast"
```

Also add a step after the base image is written to build the fast initrd:

```bash
echo "Building fast initrd..."
"$SCRIPT_DIR/../images/kernel/build-initrd.sh" /var/lib/agentiso/initrd-fast.img
```

**Step 3: Commit**

```bash
git add images/kernel/build-initrd.sh scripts/setup-e2e.sh
git commit -m "feat: add custom minimal initrd builder and init-fast shim for fast boot"
```

---

## Execution Order Summary

```
Layer 1 (parallel, no deps):
  Task 1: ConfigureWorkspace protocol type     [guest-agent]
  Task 3: ZFS pool/ dataset helpers            [storage-net]
  Task 4: nftables skip remove                 [storage-net]
  Task 7: Config PoolConfig + init_mode        [workspace-core]
  Task 11: Build scripts                       [any]

Layer 2 (depends on Layer 1):
  Task 2: Guest agent handler + retry loop     [guest-agent, after Task 1]
  Task 5: VM microvm init_mode                 [vm-engine, after Task 7]
  Task 6: VsockClient configure_workspace      [vm-engine, after Task 1]

Layer 3 (depends on Layer 2):
  Task 8: Host parallelism tokio::join!        [workspace-core, after Tasks 4,6,7]
  Task 9: VmPool manager                       [workspace-core, after Task 7]

Layer 4 (depends on Layer 3):
  Task 10: Warm pool integration               [workspace-core, after Tasks 8,9]
```

## Verification

After all tasks complete:

```bash
# All unit tests pass
cargo test

# Build guest agent
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# Re-run e2e setup (needs sudo)
sudo ./scripts/setup-e2e.sh

# Run e2e tests (needs sudo)
sudo ./scripts/e2e-test.sh
```
