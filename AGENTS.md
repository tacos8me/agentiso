# agentiso Team Agents

## Team Structure (5-agent swarm)

### 1. vm-engine
**Role**: VM Manager & QEMU Integration
**Scope**: `src/vm/`, `src/config.rs`
**Responsibilities**:
- QEMU microvm process spawning and lifecycle
- QMP (QEMU Machine Protocol) client over Unix socket
- vsock connection management to guest agents
- microvm command-line argument generation
- VM resource allocation (vCPUs, memory)
- Process monitoring and cleanup

**Key files**: `src/vm/mod.rs`, `src/vm/qemu.rs`, `src/vm/microvm.rs`, `src/vm/vsock.rs`, `src/config.rs`

### 2. storage-net
**Role**: Storage (ZFS) & Networking
**Scope**: `src/storage/`, `src/network/`
**Responsibilities**:
- ZFS zvol creation, snapshot, clone, rollback, destroy operations
- ZFS dataset layout management (base images, workspaces, forks)
- TAP device creation and bridge attachment
- nftables rule generation for per-workspace isolation
- IP allocation from 10.42.0.0/16 pool
- Port forwarding via DNAT rules
- Bridge initialization (`br-agentiso`)

**Key files**: `src/storage/mod.rs`, `src/storage/zfs.rs`, `src/network/mod.rs`, `src/network/bridge.rs`, `src/network/nftables.rs`, `src/network/dhcp.rs`

### 3. workspace-core
**Role**: Workspace Lifecycle & Snapshot Management
**Scope**: `src/workspace/`, `src/main.rs`
**Responsibilities**:
- Workspace CRUD operations
- Lifecycle state machine (Stopped -> Running -> Suspended, etc.)
- Snapshot tree management (create, restore, delete, list)
- Fork/clone workspace from snapshot
- State persistence (JSON state file)
- Orchestrating VM, storage, and network managers during lifecycle transitions
- Main binary entry point and CLI argument parsing

**Key files**: `src/workspace/mod.rs`, `src/workspace/snapshot.rs`, `src/main.rs`

### 4. mcp-server
**Role**: MCP Server & Tool Definitions
**Scope**: `src/mcp/`
**Responsibilities**:
- MCP server setup (stdio transport via rmcp)
- All MCP tool definitions and JSON schemas
- Tool handler dispatch to workspace manager
- Session-based access controls and ownership enforcement
- Resource quota enforcement
- Request validation and error responses

**Key files**: `src/mcp/mod.rs`, `src/mcp/tools.rs`, `src/mcp/auth.rs`

### 5. guest-agent
**Role**: In-VM Guest Agent
**Scope**: `guest-agent/`, `src/guest/`, `images/`
**Responsibilities**:
- Guest agent binary (vsock listener on port 5000)
- Command execution inside VM (exec with stdout/stderr/exit code)
- File read/write/upload/download inside VM
- vsock protocol implementation (length-prefixed JSON)
- Readiness handshake with host
- Protocol type definitions shared between host and guest
- Alpine base image build script
- Custom kernel build script

**Key files**: `guest-agent/src/main.rs`, `src/guest/mod.rs`, `src/guest/protocol.rs`, `images/build-alpine.sh`, `images/kernel/build-kernel.sh`

## Dependency Order

```
guest-agent (protocol types) ← vm-engine (vsock client)
storage-net (ZFS + network) ← workspace-core (orchestration)
vm-engine ← workspace-core (orchestration)
workspace-core ← mcp-server (tool handlers call workspace manager)
```

Build order: guest-agent protocol types → vm-engine + storage-net (parallel) → workspace-core → mcp-server

## Shared Interfaces

All agents should agree on these trait interfaces early:

- `VmManager` trait (vm-engine exposes, workspace-core consumes)
- `StorageManager` trait (storage-net exposes, workspace-core consumes)
- `NetworkManager` trait (storage-net exposes, workspace-core consumes)
- `GuestProtocol` types (guest-agent defines, vm-engine and guest-agent both use)
- `Workspace` / `Snapshot` structs (workspace-core defines, everyone uses)
