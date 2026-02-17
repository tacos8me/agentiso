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
**Scope**: `guest-agent/`, `agentiso/src/guest/`, `images/`
**Responsibilities**:
- Guest agent binary (vsock listener on port 5000)
- Command execution inside VM (exec with stdout/stderr/exit code)
- File read/write/upload/download inside VM
- vsock protocol implementation (length-prefixed JSON)
- Readiness handshake with host
- Protocol type definitions shared between host and guest
- Alpine base image build script
- Custom kernel build script

**Key files**: `guest-agent/src/main.rs`, `agentiso/src/guest/mod.rs`, `agentiso/src/guest/protocol.rs`, `images/build-alpine.sh`, `images/kernel/build-kernel.sh`

### 2. `vm-engine`
**Role**: VM Manager & QEMU Integration
**Scope**: `agentiso/src/vm/`
**Responsibilities**:
- QEMU microvm process spawning and lifecycle
- QMP (QEMU Machine Protocol) client over Unix socket
- vsock connection management to guest agents
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
- All MCP tool definitions and JSON schemas (20 tools)
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
- `GuestRequest`/`GuestResponse` types (guest-agent defines, vm-engine and guest-agent both use)
- `Workspace` / `Snapshot` structs (workspace-core defines, everyone uses)

## Current Status (Round 3)

**Completed (Rounds 1-2 + vsock fix)**:
- All module scaffolding and type definitions
- 172 unit tests passing, 0 warnings
- 14 e2e tests passing end-to-end (ZFS, networking, QEMU, vsock, snapshots)
- Guest agent binary fully working: vsock listener, exec, file read/write/upload/download, network config, hostname, shutdown
- Guest agent self-loads vsock kernel modules with retry + fallback to TCP
- VsockStream: custom AsyncRead/AsyncWrite over AsyncFd<OwnedFd> (cannot use tokio UnixStream for AF_VSOCK)
- QMP client, microvm command builder, VM manager scaffolding
- ZFS operations: create, clone, snapshot, destroy, list
- Network: TAP creation, bridge attach, nftables rule generation, IP allocation (DHCP pool)
- MCP tool definitions and parameter parsing (20 tools)
- Session-based auth and quota enforcement
- Workspace state machine and snapshot tree
- Config struct and validation
- E2e setup script: Alpine rootfs, kernel+initrd, vsock modules, ZFS base image

**Round 3 goals — integration testing & polish**:
All modules have full implementations. Round 3 focus: validate integration, find bugs, fix them.
- Each agent: review own module for correctness, edge cases, and compatibility with other modules
- Build task list of integration issues, missing error handling, and test gaps
- Fix graceful shutdown (microvm ACPI powerdown times out in e2e test 9)
- Validate full lifecycle: MCP tool → workspace manager → VM+storage+network → guest agent → response
- Add integration tests that exercise cross-module paths
- Ensure `cargo test` stays green (172+ tests, 0 warnings)
