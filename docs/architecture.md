# Architecture

Technical architecture reference for agentiso, a QEMU microvm workspace manager
for AI agents exposed via MCP tools.

## Overview

```
                          MCP (stdio, JSON-RPC)
                    +-----------------------------+
                    |                             |
  +-------------+   |   +---------------------+   |   +-----------------+
  |             |   |   |                     |   |   |                 |
  |  MCP Client |<--+-->|  agentiso daemon    |   |   |  QEMU microvm  |
  |  (AI agent) |       |                     |   |   |                 |
  |             |       |  +---------------+  |   |   |  +-----------+  |
  +-------------+       |  | MCP Server    |  |   |   |  | guest     |  |
                        |  +-------+-------+  |   |   |  | agent     |  |
                        |          |          |   |   |  | (vsock    |  |
                        |  +-------v-------+  |   |   |  |  :5000)   |  |
                        |  | Workspace Mgr |  |   |   |  +-----------+  |
                        |  +--+----+----+--+  |   |   |                 |
                        |     |    |    |     |   |   |  Alpine Linux   |
                        |     |    |    |     |   |   |  on ZFS zvol    |
  +----------+     +----+--+  | +--+--+ |  +--+--+---+--+---------+----+
  |          |     |       |  | |     | |  |     |   |            |
  | nftables |<--->| Net   |  | | VM  | |  | Stor|   | vsock     |
  | rules    |     | Mgr   |  | | Mgr | |  | Mgr |   | (AF_VSOCK)|
  |          |     |       |  | |     | |  |     |   |            |
  +----------+     +---+---+  | +--+--+ |  +--+--+   +------------+
                       |      |    |    |     |
                   +---+---+  |  +-+--+ |  +--+---+
                   | TAP   |  |  |QMP | |  | ZFS  |
                   | br-   |  |  |sock| |  | zvols|
                   |agentiso| |  +----+ |  +------+
                   +-------+  +--------+
```

## Module Overview

### mcp (`src/mcp/`)

MCP server over stdio transport using the rmcp crate. Defines 34 MCP tools for
workspace lifecycle, command execution, file I/O, snapshots, networking, git,
vault operations, and session management. Snapshot and vault operations are
exposed as bundled tools with an `action` parameter. Handles JSON-RPC dispatch,
parameter validation, and session-based access controls with per-session ownership.

### workspace (`src/workspace/`)

Workspace lifecycle state machine (Stopped, Running, Suspended) and snapshot
tree management. Orchestrates the VM, storage, and network managers during
lifecycle transitions (create, destroy, start, stop, fork). Owns the
`PersistedState` struct and atomic state file writes. Also contains the warm VM
pool and the batch orchestration engine for parallel task execution. Tracks fork
lineage (source workspace and snapshot name) for each forked workspace.

### vm (`src/vm/`)

QEMU microvm process spawning and lifecycle management. Includes the QMP
(QEMU Machine Protocol) client over Unix socket with per-command 10-second
timeouts and exponential backoff on connect. Manages vsock connections to
in-VM guest agents with reconnect support for idempotent operations. Detects
VM crashes and captures console log diagnostics on boot failure.

### storage (`src/storage/`)

ZFS operations via shell-out to `zfs`/`zpool` CLI commands. Handles zvol
creation, snapshot, clone, rollback, and destroy. Manages the dataset layout
hierarchy (base images, workspaces, forks) with LZ4 compression. Enforces
per-workspace volsize quotas and pool space hard-fail guards.

### network (`src/network/`)

TAP device creation, bridge attachment (`br-agentiso`), and per-workspace
nftables rule generation. Sequential IP allocation from the 10.99.0.0/16
pool. Port forwarding via DNAT rules (inet family). Default isolation blocks
inter-VM traffic; internet and port allowlists are configurable per workspace.

### guest (`src/guest/`, `protocol/`, `guest-agent/`)

Guest agent protocol types live in the shared `agentiso-protocol` crate,
consumed by both the host daemon and the in-VM guest agent. The guest agent
binary (`agentiso-guest`) is statically linked with musl and listens on vsock
port 5000 inside each VM. It handles exec (foreground and background), file
read/write/upload/download, network configuration, hostname, env injection,
and graceful shutdown. Hardened with file size limits (32 MiB), exec timeouts,
hostname/IP validation, and process group isolation for reliable kill.

## Communication: vsock Protocol

Host and guest communicate over AF_VSOCK (virtio-socket), not SSH or TCP.
Each VM is assigned a unique CID (Context ID) starting from 100.

### Framing

Each message is length-prefixed:

```
+-------------------+-----------------------------+
| 4 bytes (BE u32)  |  JSON payload (UTF-8)       |
| payload length    |                             |
+-------------------+-----------------------------+
```

The 4-byte big-endian prefix encodes the size of the JSON payload only (not
including the prefix itself). Maximum message size is 16 MiB.

### Protocol Types

Request messages (`GuestRequest`) use tagged JSON (`"type"` field):

- `Ping` -- readiness handshake
- `Exec` -- synchronous command execution with timeout
- `ExecBackground` -- start command, return job ID
- `ExecPoll` -- check background job status
- `ExecKill` -- kill background job by process group
- `FileRead`, `FileWrite`, `FileUpload`, `FileDownload` -- file I/O
- `ConfigureWorkspace` -- set hostname + network in one shot
- `SetEnv` -- inject environment variables (API keys via vsock, never disk)
- `Shutdown` -- graceful VM shutdown

Response messages (`GuestResponse`) mirror each request type with result or
error payloads.

### Connection Lifecycle

1. QEMU boots with `vhost-vsock-device,guest-cid={cid}`
2. Guest agent starts via OpenRC, listens on vsock port 5000
3. Host connects to `(cid, 5000)` and sends `Ping`
4. Guest responds with `Pong` -- workspace is ready
5. Subsequent operations open new vsock connections as needed
6. Idempotent operations (file read, exec) support automatic reconnect

## Storage: ZFS Layout

### Dataset Hierarchy

```
{pool}/{prefix}/
  base/
    alpine-dev@latest          <- base image snapshot (read-only)
  workspaces/
    ws-{uuid1}                 <- zvol, cloned from base@latest
      @checkpoint-1            <- named snapshot
      @checkpoint-2
    ws-{uuid2}
  forks/
    ws-{uuid3}                 <- cloned from ws-{uuid1}@checkpoint-1
```

Default pool is `agentiso`, prefix is `agentiso`, so the full path is
`agentiso/agentiso/base/alpine-dev@latest`.

### Copy-on-Write Operations

| Operation | ZFS Command | Time |
|-----------|------------|------|
| Create workspace | `zfs clone base/alpine-dev@latest workspaces/ws-{id}` | ~instant |
| Snapshot | `zfs snapshot workspaces/ws-{id}@{name}` | ~instant |
| Restore | `zfs rollback workspaces/ws-{id}@{name}` | ~1s |
| Fork | `zfs clone workspaces/ws-{id}@{snap} forks/ws-{new_id}` | ~instant |
| Destroy | `zfs destroy -r workspaces/ws-{id}` | fast |

All workspaces use LZ4 compression. Per-workspace volsize quotas are enforced
at creation time.

## Networking

### Topology

```
Host
  br-agentiso (10.99.0.1/16)
    +-- tap-{ws1} --> VM1 (10.99.0.2)
    +-- tap-{ws2} --> VM2 (10.99.0.3)
    +-- ...
```

### Per-Workspace Setup

1. Create TAP device: `ip tuntap add tap-{short_id} mode tap`
2. Attach to bridge: `ip link set tap-{short_id} master br-agentiso`
3. Bring up: `ip link set tap-{short_id} up`
4. QEMU binds TAP as virtio-net-device
5. Guest agent configures IP + gateway + DNS via `ConfigureWorkspace` RPC
6. nftables rules installed for isolation

### Isolation (nftables)

- Inter-VM traffic blocked by default
- Host-to-VM always allowed (vsock + bridge)
- Internet access via NAT, configurable per workspace
- Port forwarding: host DNAT rules (must use `dnat ip to` for inet family)

### IP Allocation

Sequential allocation from 10.99.0.0/16 (starting at .2). IPs tracked in
workspace state and recycled on destroy. Supports ~65k concurrent workspaces.

## State Persistence

### State File

In-memory state is periodically serialized to a JSON file (default:
`/var/lib/agentiso/state.json`). The persist interval is configurable
(default 30 seconds). State is also saved on every lifecycle transition
(create, destroy, stop, start, fork, snapshot).

### Atomic Write

State is written atomically using the rename pattern:

1. Serialize `PersistedState` to JSON
2. Write to `state.json.tmp`
3. `rename()` to `state.json` (atomic on same filesystem)
4. Set permissions to 0600

### Schema Versioning

The `PersistedState` struct includes a `schema_version` field:

- Version 0: legacy (no version field)
- Version 1: workspace state + vsock CID tracking
- Version 2: adds session ownership persistence

Unknown future versions log a warning but attempt to load (forward-compatible
via `serde(default)`).

### Auto-Adopt and Orphan Reconciliation on Restart

When the daemon starts and loads persisted state, it performs full
reconciliation and **auto-adopts** running workspaces:

1. Scans for running QEMU processes and re-attaches to any that belong to
   known workspaces (auto-adopt)
2. Kills orphaned QEMU processes not matching any known workspace
3. Destroys stale TAP devices for loaded workspaces (TAP cannot survive restart)
4. Destroys orphaned TAP devices not matching any known workspace
5. Detects orphaned ZFS datasets not tracked in state (logs warnings)
6. Marks workspaces whose VMs are no longer running as Stopped

Auto-adopt eliminates the need for manual `workspace_adopt` calls in most
restart scenarios. Agents can still use `workspace_adopt` or
`workspace_adopt_all` for workspaces that were stopped during the restart
window.

## Warm VM Pool

The warm VM pool (enabled by default) pre-boots VMs in the background so that
`workspace_create` can skip the boot wait and return in sub-second time.

### Configuration (`[pool]` section)

| Key | Default | Description |
|-----|---------|-------------|
| `enabled` | `true` | Enable the pool |
| `min_size` | `2` | Minimum VMs to keep ready |
| `max_size` | `10` | Maximum VMs in the pool |
| `target_free` | `3` | Target ready (unassigned) VMs |
| `max_memory_mb` | `8192` | Total memory budget for pool VMs |

### How It Works

1. A background task monitors the pool and boots new VMs when the ready count
   drops below `target_free`
2. Each warm VM is a fully booted QEMU microvm with guest agent ready, TAP
   device attached, and ZFS zvol cloned from the base image
3. On `workspace_create`, the manager claims a warm VM from the FIFO queue
   instead of booting from scratch
4. The claimed VM's ZFS dataset, TAP device, and IP are transferred to the
   new workspace
5. After a VM is claimed, the pool automatically boots a replacement to
   maintain the target count (auto-replenish)
6. On shutdown, all unclaimed pool VMs are drained and destroyed
