# agentiso — QEMU microvm Workspace Manager for AI Agents

**Date**: 2026-02-16
**Status**: Approved

## Overview

agentiso is a Rust MCP server that manages QEMU microvm-based isolated, snapshotable workspaces for any AI agent. It exposes workspace lifecycle, command execution, file transfer, and network control as MCP tools.

## Requirements

- **Runtime**: QEMU `-M microvm` with custom kernel, Alpine-based dev images
- **Scale**: 20+ concurrent VMs on server-class hardware
- **Snapshots**: Named checkpoints + fork/clone (full snapshot tree)
- **MCP Surface**: Lifecycle + exec + file I/O + network control with access controls
- **Networking**: TAP + bridge with per-workspace nftables isolation
- **Storage**: ZFS zvols for instant snapshots and clones
- **Agent Interface**: Shell + network services via vsock guest agent
- **Language**: Rust (monolithic binary)

## Architecture: Monolithic Daemon

```
agentiso (single Rust binary)
├── MCP Server (stdio transport)
├── VM Manager (QEMU microvm + QMP + vsock)
├── Network Manager (TAP/bridge/nftables)
├── Storage Manager (ZFS snapshots & clones)
└── Exec Proxy (vsock guest agent protocol)
```

## Data Model

### Workspace

```rust
struct Workspace {
    id: Uuid,
    name: String,
    state: WorkspaceState,        // Running | Stopped | Suspended
    base_image: String,           // name of the Alpine template
    zfs_dataset: String,          // tank/agentiso/workspaces/ws-{id}
    qemu_pid: Option<u32>,
    qmp_socket: PathBuf,          // /run/agentiso/{id}/qmp.sock
    vsock_cid: u32,               // for host<->guest communication
    tap_device: String,           // tap-{short_id}
    network: WorkspaceNetwork,
    snapshots: Vec<Snapshot>,
    created_at: DateTime<Utc>,
    resources: ResourceLimits,    // vcpus, memory, disk quota
}
```

### Snapshot

```rust
struct Snapshot {
    id: Uuid,
    name: String,
    workspace_id: Uuid,
    zfs_snapshot: String,          // tank/agentiso/workspaces/ws-{id}@{snap_name}
    qemu_state: Option<PathBuf>,  // saved VM memory state for live snapshots
    parent: Option<Uuid>,         // forms a tree
    created_at: DateTime<Utc>,
}
```

### Lifecycle State Machine

```
create -> [Stopped]
           -> start -> [Running]
                        -> suspend -> [Suspended] -> resume -> [Running]
                        -> snapshot -> [Running] (creates checkpoint)
                        -> stop -> [Stopped]
           -> fork (from snapshot) -> new Workspace [Stopped]
           -> destroy -> removed (ZFS destroy + cleanup)
```

## MCP Tool Surface

### Lifecycle Tools

| Tool | Params | Description |
|------|--------|-------------|
| `workspace_create` | `name?, base_image?, vcpus?, memory_mb?, disk_gb?` | Create and start a new workspace |
| `workspace_destroy` | `workspace_id` | Stop and destroy workspace |
| `workspace_list` | `state_filter?` | List workspaces with status |
| `workspace_info` | `workspace_id` | Full workspace details |
| `workspace_stop` | `workspace_id` | Graceful shutdown |
| `workspace_start` | `workspace_id` | Boot a stopped workspace |

### Execution Tools

| Tool | Params | Description |
|------|--------|-------------|
| `exec` | `workspace_id, command, timeout_secs?, workdir?, env?` | Execute command via vsock agent |
| `file_write` | `workspace_id, path, content, mode?` | Write file inside VM |
| `file_read` | `workspace_id, path, offset?, limit?` | Read file from VM |
| `file_upload` | `workspace_id, host_path, guest_path` | Transfer host -> VM |
| `file_download` | `workspace_id, guest_path, host_path` | Transfer VM -> host |

### Snapshot Tools

| Tool | Params | Description |
|------|--------|-------------|
| `snapshot_create` | `workspace_id, name, include_memory?` | Create named snapshot |
| `snapshot_restore` | `workspace_id, snapshot_name` | Restore to snapshot |
| `snapshot_list` | `workspace_id` | List snapshot tree |
| `snapshot_delete` | `workspace_id, snapshot_name` | Delete snapshot |
| `workspace_fork` | `workspace_id, snapshot_name, new_name?` | Clone workspace from snapshot |

### Network Tools

| Tool | Params | Description |
|------|--------|-------------|
| `port_forward` | `workspace_id, guest_port, host_port?` | Forward host port to guest |
| `port_forward_remove` | `workspace_id, guest_port` | Remove port forward |
| `workspace_ip` | `workspace_id` | Get VM IP address |
| `network_policy` | `workspace_id, allow_internet?, allow_inter_vm?, allowed_ports?` | Set network isolation policy |

### Access Controls

- Session-token-based ownership: agents can only operate on workspaces they created
- Resource quotas: max VMs, max memory, max disk per session
- Network policy defaults: no inter-VM, limited internet

## VM Architecture

### QEMU microvm

```bash
qemu-system-x86_64 \
  -M microvm,rtc=on \
  -cpu host -enable-kvm \
  -m {memory_mb} -smp {vcpus} \
  -kernel /var/lib/agentiso/vmlinuz \
  -initrd /var/lib/agentiso/initrd.img \
  -append "console=hvc0 root=/dev/vda rw quiet" \
  -drive id=root,file=/dev/zvol/tank/agentiso/workspaces/ws-{id},format=raw,if=none \
  -device virtio-blk-device,drive=root \
  -netdev tap,id=net0,ifname=tap-{id},script=no,downscript=no \
  -device virtio-net-device,netdev=net0 \
  -device vhost-vsock-pci,guest-cid={cid} \
  -chardev socket,id=qmp,path={qmp_sock},server=on,wait=off \
  -mon chardev=qmp,mode=control \
  -nographic -nodefaults
```

- **microvm machine type**: Minimal device model, ~200ms boot
- **Custom vmlinux**: Stripped kernel with virtio drivers, no modules
- **ZFS zvol as raw block device**: Instant ZFS snapshots on block level
- **vsock**: Host<->guest communication without networking, faster than SSH
- **QMP socket**: VM management (pause, savevm, query status)

### Guest Agent

Static musl binary (`agentiso-guest`) inside Alpine VM:
- Listens on vsock port 5000
- Handles: exec, file_read, file_write, file_upload, file_download
- Protocol: length-prefixed JSON over vsock
- Started via OpenRC
- Reports readiness to host via vsock handshake

### Alpine Base Image

- `agentiso-guest` binary baked in
- OpenRC starts guest agent on boot
- Dev tools: build-base, git, python3, nodejs, cargo
- Dynamic config via guest agent (hostname, network)
- Stored as ZFS dataset; workspaces clone from snapshot

### Boot Sequence

1. ZFS clone from base -> new zvol
2. Create TAP device + attach to bridge
3. Configure nftables rules
4. Spawn QEMU process
5. Wait for guest agent readiness (vsock handshake)
6. Return workspace info to agent

Target: < 1 second from workspace_create to ready.

## Networking

### Topology

```
Host (agentiso)
  br-agentiso (bridge) 10.42.0.1/16
    ├── tap-ws1 -> VM1 10.42.0.2
    ├── tap-ws2 -> VM2 10.42.0.3
    └── ...
  nftables per-workspace rules
```

### Per-Workspace Setup

1. Create TAP: `ip tuntap add tap-{id} mode tap`
2. Attach to bridge: `ip link set tap-{id} master br-agentiso`
3. Assign guest IP via guest agent
4. nftables rules: block inter-VM, allow host<->VM, NAT internet

### Isolation

- Block inter-VM traffic by default (nftables)
- Allow host<->VM always (vsock + management)
- Internet via NAT, configurable per workspace
- Port allowlists for external reachability

### IP Allocation

Sequential from 10.42.0.0/16 pool (~65k addresses). Tracked in workspace state. Reclaimed on destroy.

## Storage (ZFS)

### Dataset Layout

```
tank/agentiso/
├── base/
│   └── alpine-dev@latest
├── workspaces/
│   ├── ws-{uuid1}            (zvol, cloned from base)
│   │   @checkpoint-1
│   │   @checkpoint-2
│   └── ws-{uuid2}
└── forks/
    └── ws-{uuid3}            (cloned from ws-{uuid1}@checkpoint-1)
```

### Operations

| Operation | ZFS Command | Speed |
|-----------|-------------|-------|
| Create workspace | `zfs clone base/alpine-dev@latest .../ws-{id}` | ~instant |
| Snapshot | `zfs snapshot .../ws-{id}@{name}` | ~instant |
| Restore | Stop VM -> `zfs rollback` -> Start VM | ~1s |
| Fork | `zfs clone ws-{id}@{snap} .../ws-{new_id}` | ~instant |
| Destroy | `zfs destroy -r .../ws-{id}` | fast |

### Memory Snapshots

For live snapshots (VM state + disk):
1. QMP `savevm` for QEMU memory state
2. ZFS snapshot for disk
3. Bundle as named snapshot
4. Restore: ZFS rollback + QMP `loadvm`

## Project Structure

```
agentiso/
├── Cargo.toml
├── src/
│   ├── main.rs                  # CLI entry, config loading
│   ├── config.rs                # Configuration (TOML)
│   ├── mcp/
│   │   ├── mod.rs               # MCP server setup (stdio transport)
│   │   ├── tools.rs             # Tool definitions & handlers
│   │   └── auth.rs              # Session/ownership enforcement
│   ├── vm/
│   │   ├── mod.rs               # VM manager
│   │   ├── qemu.rs              # QEMU process spawning, QMP client
│   │   ├── microvm.rs           # microvm config generation
│   │   └── vsock.rs             # vsock connection to guest agent
│   ├── storage/
│   │   ├── mod.rs               # Storage manager
│   │   └── zfs.rs               # ZFS operations
│   ├── network/
│   │   ├── mod.rs               # Network manager
│   │   ├── bridge.rs            # Bridge + TAP creation
│   │   ├── nftables.rs          # Per-workspace firewall rules
│   │   └── dhcp.rs              # IP allocation
│   ├── workspace/
│   │   ├── mod.rs               # Workspace CRUD + lifecycle state machine
│   │   └── snapshot.rs          # Snapshot tree management
│   └── guest/
│       ├── mod.rs               # Guest agent protocol
│       └── protocol.rs          # Message types for vsock protocol
├── guest-agent/
│   ├── Cargo.toml
│   └── src/
│       └── main.rs              # vsock listener, exec, file ops
├── images/
│   ├── build-alpine.sh          # Build base Alpine image
│   └── kernel/
│       └── build-kernel.sh      # Build stripped vmlinux
└── tests/
    ├── integration/
    └── unit/
```
