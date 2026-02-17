# Boot Latency Optimization Design

**Date**: 2026-02-17
**Goal**: Sub-second workspace assignment from warm pool; ~1-2s cold boot.

## Problem

Current boot path takes 5-20 seconds, 95%+ in guest boot:
- Host distro initrd is ~70MB (kernel decompresses all before pivot root)
- OpenRC runs 3 sequential runlevels (sysinit -> boot -> default)
- Guest agent has unconditional 1s sleep after vsock module load
- No VM pre-warming

## Architecture

Four independent optimization layers:

### 1. Custom Minimal Initrd (~1MB)

Replace the 70MB host initrd with a ~1MB custom initrd containing:
- **busybox** (2.1MB uncompressed, ~880KB gzip'd) for mount, switch_root, insmod
- **3 vsock kernel modules** (~240KB decompressed): vsock.ko, vmw_vsock_virtio_transport_common.ko, vmw_vsock_virtio_transport.ko
- **Tiny init script** that loads modules, mounts root, switch_roots

Note: virtio_blk, virtio_net, and virtio_console are **built-in** to the host kernel (confirmed via `modules.builtin` and `/boot/config-*`). Only vsock modules are needed.

**Build script**: `images/kernel/build-initrd.sh`
**Output**: `/var/lib/agentiso/initrd-fast.img`

Init script inside initrd (`/init`):
```sh
#!/bin/busybox sh
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
# vsock modules (host kernel has CONFIG_MODULE_COMPRESS_ZSTD=y)
/bin/busybox insmod /lib/modules/vsock.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport_common.ko 2>/dev/null
/bin/busybox insmod /lib/modules/vmw_vsock_virtio_transport.ko 2>/dev/null
# Mount root and switch
/bin/busybox mount -t ext4 /dev/vda /mnt/root
exec /bin/busybox switch_root /mnt/root /sbin/init-fast
```

### 2. Init Bypass (Shell Shim)

Replace OpenRC with a minimal init shim at `/sbin/init-fast` in the Alpine rootfs. The guest agent cannot be PID 1 directly because it:
- Has no zombie reaping (SIGCHLD handling)
- Has no filesystem mounting logic
- Reads `/sys/class/vsock/local_cid` (needs sysfs mounted)

The shim (`/sbin/init-fast`):
```sh
#!/bin/sh
# Mount essential filesystems (if not already from initrd)
mountpoint -q /proc || mount -t proc proc /proc
mountpoint -q /sys  || mount -t sysfs sysfs /sys
mountpoint -q /dev  || mount -t devtmpfs devtmpfs /dev
mount -t tmpfs tmpfs /tmp
mount -t tmpfs tmpfs /run
# Ensure vsock modules loaded (may already be from initrd)
insmod /lib/modules/$(uname -r)/kernel/net/vmw_vsock/vsock.ko 2>/dev/null
insmod /lib/modules/$(uname -r)/kernel/net/vmw_vsock/vmw_vsock_virtio_transport_common.ko 2>/dev/null
insmod /lib/modules/$(uname -r)/kernel/net/vmw_vsock/vmw_vsock_virtio_transport.ko 2>/dev/null
# Reap zombies in background
trap 'wait' CHLD
# Exec guest agent (becomes main process under PID 1 shell)
exec /usr/local/bin/agentiso-guest
```

Kernel command line switches init: `init=/sbin/init-fast`

The base image retains OpenRC for interactive/debug use (`init=/sbin/init` or no `init=` override).

### 3. Guest Agent Fixes

**Remove unconditional 1s sleep** (guest-agent/src/main.rs ~line 899):
```rust
// BEFORE: unconditional 1s sleep after load_vsock_modules()
load_vsock_modules();
tokio::time::sleep(Duration::from_secs(1)).await;

// AFTER: tight retry loop with short intervals
load_vsock_modules();
for _ in 0..20 {
    if let Ok(listener) = try_vsock_bind(port) {
        return Ok(listener);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}
// fall back to TCP
```

**Add `ConfigureWorkspace` batched message** — combines network config + hostname in one vsock RTT:
```rust
// New variant in GuestRequest enum (protocol.rs):
ConfigureWorkspace(WorkspaceConfig),

// New struct:
pub struct WorkspaceConfig {
    pub ip_address: String,
    pub gateway: String,
    pub dns: Vec<String>,
    pub hostname: String,
}
```

### 4. Adaptive Warm VM Pool

#### Pool Structure

```rust
pub struct VmPool {
    ready: VecDeque<WarmVm>,    // booted, guest agent responding
    booting: Vec<BootingVm>,     // cold-booting in background
    config: PoolConfig,
}

pub struct PoolConfig {
    pub enabled: bool,
    pub min_size: usize,         // always maintain (default: 2)
    pub max_size: usize,         // never exceed (default: 10)
    pub target_free: usize,      // try to keep available (default: 3)
    pub max_memory_mb: usize,    // total memory budget (default: 8192)
}

pub struct WarmVm {
    pub id: Uuid,                // warm VM identifier
    pub vsock_cid: u32,          // pre-allocated CID
    pub zfs_dataset: String,     // pool/warm-{uuid}
    pub zvol_path: PathBuf,      // /dev/zvol/.../pool/warm-{uuid}
    pub qemu_pid: u32,
    pub qmp_socket: PathBuf,
    pub booted_at: Instant,
}
```

#### Storage Strategy

Warm VMs boot on ZFS clones in a `pool/` namespace:
```
agentiso/agentiso/pool/warm-{uuid-1}    (pre-booted)
agentiso/agentiso/pool/warm-{uuid-2}    (pre-booted)
```

On assignment, the workspace's `zfs_dataset` field points directly to the `pool/warm-{uuid}` path. **No `zfs rename` needed** — QEMU already has the zvol open by fd (major:minor), and the `Workspace.zfs_dataset` field is already a free-form string. Subsequent snapshots, forks, and destroys all work against whatever path is stored in `zfs_dataset`.

Validation confirmed: `zfs rename` works on active zvols (QEMU survives), but skipping rename is simpler and has zero edge cases.

#### Warm VM Lifecycle

1. **Server startup**: Begin cold-booting `min_size` VMs from base snapshot into `pool/`.
2. **Background replenisher**: Tokio task monitors pool. When `ready.len() < target_free` and total pool < `max_size`, start cold boots.
3. **Assignment** (the fast path, <200ms):
   - Pop `WarmVm` from `ready` queue
   - `tokio::join!`: TAP/nftables setup || IP allocation
   - vsock `ConfigureWorkspace` (IP + hostname, single RTT)
   - Build `Workspace` struct with `zfs_dataset = warm_vm.zfs_dataset`
   - Return workspace handle
4. **Pool empty fallback**: If no warm VMs available, fall back to cold create (custom initrd + init bypass = ~1-2s).
5. **Destroy**: Normal destroy path — destroy zvol (whether under `workspaces/` or `pool/`).

#### Warm VM Boot Configuration

Warm VMs boot with:
- **No TAP device** (no network until assignment — saves TAP resources)
- **Pre-allocated vsock CID** from the normal CID pool
- **Base snapshot clone** in `pool/` namespace
- **Fast init** (`init=/sbin/init-fast`)
- **Custom initrd** (`initrd-fast.img`)

### 5. Host-Side Parallelism

**`tokio::join!` for ZFS clone + TAP setup** (workspace/mod.rs, saves 50-200ms):
```rust
let (storage_result, network_result) = tokio::join!(
    self.storage.create_workspace(&base_image, &snapshot, &short_id),
    async { self.network.write().await.setup_workspace(&short_id, &policy).await }
);
```

**Skip `remove_workspace_rules()` for new workspaces** (nftables.rs, saves 15-40ms):
The current code unconditionally calls `remove_workspace_rules()` before applying new rules, which runs 3 `nft -a list chain` subprocesses even for brand-new workspace IDs that have no existing rules.

### 6. Config Changes

New fields in `config.toml`:
```toml
[pool]
enabled = true
min_size = 2
max_size = 10
target_free = 3
max_memory_mb = 8192

[vm]
init_mode = "fast"              # "fast" = init-fast, "openrc" = standard Alpine
initrd_path = "/var/lib/agentiso/initrd-fast.img"
```

## Performance Budget

| Path | Target | Breakdown |
|------|--------|-----------|
| **Warm assign** | <200ms | Pop VM (~0ms) + TAP+nftables (~100ms) + vsock configure (~50ms) + metadata (~5ms) |
| **Cold boot (fast)** | ~1-2s | ZFS clone (~50ms) + TAP (~100ms) + QEMU spawn (~50ms) + kernel+initrd (~300ms) + init-fast (~100ms) + agent ready (~200ms) |
| **Cold boot (OpenRC)** | ~5-20s | Same as current (fallback mode) |

## Files Changed

| File | Change |
|------|--------|
| `images/kernel/build-initrd.sh` | **NEW** — build custom ~1MB initrd |
| `scripts/setup-e2e.sh` | Add init-fast shim to rootfs, run build-initrd.sh |
| `guest-agent/src/main.rs` | Remove 1s sleep, add ConfigureWorkspace handler |
| `agentiso/src/guest/protocol.rs` | Add ConfigureWorkspace request/response |
| `agentiso/src/vm/mod.rs` | Add pool integration, init_mode in VmConfig |
| `agentiso/src/vm/microvm.rs` | Conditional init= in kernel cmdline |
| `agentiso/src/workspace/mod.rs` | Warm pool claim path, parallel ZFS+TAP, batched vsock config |
| `agentiso/src/workspace/pool.rs` | **NEW** — VmPool manager |
| `agentiso/src/storage/zfs.rs` | Add pool_dataset(), pool/ in ensure_pool_layout() |
| `agentiso/src/storage/mod.rs` | Add pool storage helpers |
| `agentiso/src/network/nftables.rs` | Skip remove on new workspaces |
| `agentiso/src/config.rs` | Add PoolConfig, init_mode |
