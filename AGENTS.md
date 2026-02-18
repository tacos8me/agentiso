# agentiso Team Agents

## Activation

To reactivate the swarm, create a team named `agentiso-swarm` and spawn 5 teammates using the Task tool with `team_name: "agentiso-swarm"`. Each agent should be `subagent_type: "general-purpose"` with `mode: "bypassPermissions"`.

Agent names (use these exact names):
1. `guest-agent`
2. `vm-engine`
3. `storage-net`
4. `workspace-core`
5. `mcp-server`

Each agent's prompt should instruct them to read this file and the design doc, then claim their scoped files. Set up task dependencies per the dependency order below. Agents 1, 2, 3 can start in parallel. Agent 4 waits on 2+3. Agent 5 waits on 4.

## Team Structure (5-agent swarm)

### 1. `guest-agent`
**Role**: In-VM Guest Agent & Protocol Types
**Scope**: `protocol/`, `guest-agent/`, `agentiso/src/guest/`, `images/`
**Responsibilities**:
- Guest agent binary (vsock listener on port 5000)
- Command execution inside VM (exec with stdout/stderr/exit code, background jobs, ExecKill by job_id)
- File read/write/upload/download inside VM
- vsock protocol implementation (length-prefixed JSON)
- Readiness handshake with host
- Protocol type definitions in shared `agentiso-protocol` crate (`protocol/`), consumed by both host and guest agent
- Alpine base image build script
- Custom kernel build script

**Key files**: `protocol/src/lib.rs`, `guest-agent/src/main.rs`, `agentiso/src/guest/mod.rs`, `agentiso/src/guest/protocol.rs`, `images/build-alpine.sh`, `images/kernel/build-kernel.sh`

### 2. `vm-engine`
**Role**: VM Manager & QEMU Integration
**Scope**: `agentiso/src/vm/`
**Responsibilities**:
- QEMU microvm process spawning and lifecycle
- QMP (QEMU Machine Protocol) client over Unix socket with per-command 10s timeout and exponential backoff on connect
- VM crash detection (check_vm_alive) and console log diagnostics on boot failure
- vsock connection management to guest agents with reconnect for idempotent operations
- microvm command-line argument generation
- VM resource allocation (vCPUs, memory)
- Process monitoring and cleanup

**Key files**: `agentiso/src/vm/mod.rs`, `agentiso/src/vm/qemu.rs`, `agentiso/src/vm/microvm.rs`, `agentiso/src/vm/vsock.rs`

### 3. `storage-net`
**Role**: Storage (ZFS) & Networking
**Scope**: `agentiso/src/storage/`, `agentiso/src/network/`
**Responsibilities**:
- ZFS zvol creation, snapshot, clone, rollback, destroy operations
- ZFS dataset layout management (base images, workspaces, forks)
- TAP device creation and bridge attachment
- nftables rule generation for per-workspace isolation
- IP allocation from 10.42.0.0/16 pool
- Port forwarding via DNAT rules
- Bridge initialization (`br-agentiso`)

**Key files**: `agentiso/src/storage/mod.rs`, `agentiso/src/storage/zfs.rs`, `agentiso/src/network/mod.rs`, `agentiso/src/network/bridge.rs`, `agentiso/src/network/nftables.rs`, `agentiso/src/network/dhcp.rs`

### 4. `workspace-core`
**Role**: Workspace Lifecycle & Snapshot Management
**Scope**: `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs`
**Responsibilities**:
- Workspace CRUD operations
- Lifecycle state machine (Stopped -> Running -> Suspended, etc.)
- Snapshot tree management (create, restore, delete, list)
- Fork/clone workspace from snapshot
- State persistence (JSON state file)
- Orchestrating VM, storage, and network managers during lifecycle transitions
- Main binary entry point and CLI argument parsing
- Configuration struct and TOML loading

**Key files**: `agentiso/src/workspace/mod.rs`, `agentiso/src/workspace/snapshot.rs`, `agentiso/src/config.rs`, `agentiso/src/main.rs`

### 5. `mcp-server`
**Role**: MCP Server & Tool Definitions
**Scope**: `agentiso/src/mcp/`
**Responsibilities**:
- MCP server setup (stdio transport via rmcp)
- All MCP tool definitions and JSON schemas (28 tools, including workspace_logs and exec_kill)
- Tool handler dispatch to workspace manager
- Session-based access controls and ownership enforcement
- Resource quota enforcement
- Request validation and error responses

**Key files**: `agentiso/src/mcp/mod.rs`, `agentiso/src/mcp/tools.rs`, `agentiso/src/mcp/auth.rs`

## Dependency Order

```
guest-agent (protocol types) <- vm-engine (vsock client)
storage-net (ZFS + network)  <- workspace-core (orchestration)
vm-engine                    <- workspace-core (orchestration)
workspace-core               <- mcp-server (tool handlers call workspace manager)
```

Build order: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

## Shared Interfaces

All agents should agree on these trait interfaces early:

- `VmManager` struct (vm-engine exposes, workspace-core consumes)
- `StorageManager` struct (storage-net exposes, workspace-core consumes)
- `NetworkManager` struct (storage-net exposes, workspace-core consumes)
- `GuestRequest`/`GuestResponse` types (defined in shared `agentiso-protocol` crate, used by host and guest-agent; `agentiso/src/guest/protocol.rs` re-exports from crate)
- `Workspace` / `Snapshot` structs (workspace-core defines, everyone uses)

## Current Status (Round 6)

**Completed (Rounds 1-6)**:
- All module scaffolding and type definitions
- 278 unit tests passing, 0 warnings
- 14 e2e tests passing end-to-end (ZFS, networking, QEMU, vsock, snapshots)
- 14/14 MCP integration test steps passing (full lifecycle)
- Guest agent binary fully working: vsock listener, exec, file read/write/upload/download, network config, hostname, shutdown
- Guest agent self-loads vsock kernel modules with retry + fallback to TCP
- VsockStream: custom AsyncRead/AsyncWrite over AsyncFd<OwnedFd> (cannot use tokio UnixStream for AF_VSOCK)
- QMP client, microvm command builder, VM manager scaffolding
- ZFS operations: create, clone, snapshot, destroy, list (with LZ4 compression)
- Network: TAP creation, bridge attach, nftables rule generation, IP allocation (DHCP pool)
- MCP tool definitions and parameter parsing (28 tools)
- Session-based auth and quota enforcement
- Workspace state machine, snapshot tree, and name uniqueness checks
- Config struct and validation (with InitMode enum)
- CLI: `agentiso check`, `agentiso status` (with PID liveness check and created_at column), `agentiso logs <id>` (console/stderr log viewer)
- E2e setup script: Alpine rootfs, kernel+initrd, vsock modules, ZFS base image
- Example config: `config.example.toml` with all sections documented
- Security hardening: file size limits, hostname/IP validation, HMP sanitization, UTF-8 safe truncation, path traversal prevention
- Wave 4 — Reliability/VM health: per-QMP-command timeout (10s), exponential backoff on QMP connect, VM crash detection, console log diagnostics on boot failure, vsock reconnect for idempotent operations
- Wave 5 — Protocol/DX: ExecKill protocol variant and guest handler, exec_kill MCP tool, workspace_logs MCP tool, configure_network retry in guest agent (28 MCP tools total)
- Round 6 — Bugfix: removed `refquota` from ZFS zvol clones (filesystem-only property, invalid for zvols). Zvols inherit `volsize` from parent snapshot.
