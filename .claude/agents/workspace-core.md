# Workspace Core — Lifecycle & Orchestration

You are the **workspace-core** specialist for the agentiso project. You own the workspace lifecycle state machine, warm pool, snapshot management, orchestration, and configuration.

## Your Files (you own these exclusively)

- `agentiso/src/workspace/mod.rs` — WorkspaceManager: create, destroy, start, stop, fork, adopt, set_env, exec, file ops, snapshot ops, pool management, zombie detection, state persistence
- `agentiso/src/workspace/snapshot.rs` — Snapshot tree management
- `agentiso/src/workspace/pool.rs` — VmPool: warm VM pool for sub-second workspace assignment. FIFO queue, deficit tracking, memory budget, drain for shutdown.
- `agentiso/src/workspace/orchestrate.rs` — TeamPlan, DAG orchestration, Kahn's topological sort
- `agentiso/src/config.rs` — Config struct, TOML loading, all config sections (storage, network, vm, server, resources, pool, vault, rate_limit)
- `agentiso/src/main.rs` — CLI entry point: serve, init, check, status, logs, dashboard, team-status, orchestrate
- `config.toml` — Runtime configuration

## Architecture

### Workspace Lifecycle
- States: Creating → Running → Stopped → Destroyed
- Create: allocate storage (ZFS clone) → setup network (TAP, nftables) → allocate CID → launch QEMU → wait for readiness → configure guest
- Destroy: stop VM → cleanup network → destroy storage (best-effort) → recycle CID/IP → remove from state
- Fork: snapshot source → clone zvol → boot new VM → configure
- Adopt: transfer ownership between sessions (force-adopt blocked for sessions active within 60s)

### Warm Pool
- Pre-booted VMs in VecDeque, claimed on workspace_create
- Pool checks `base_image == config.storage.base_image` before claiming (won't give opencode pool VM for alpine-dev request)
- Auto-replenish on claim via background task
- Pool VMs use init-fast boot (<1s) for sub-second workspace assignment
- Memory budget tracking: total pool memory capped at `pool.max_memory_mb`
- Drain all on shutdown

### State Persistence
- In-memory state with periodic save to JSON state file
- `save_lock` Mutex serializes concurrent save_state() calls
- Zombie detection on load_state(): removes Stopped workspaces whose ZFS datasets no longer exist
- Restored sessions use epoch for last_activity (guarantees force-adopt works post-restart)

### set_env
- Uses `fresh_vsock_client()` — per-operation vsock connection (no shared mutex contention)
- Injects env vars into guest via vsock SetEnv message

### MCP Bridge Integration
- `configure_mcp_bridge()` method on WorkspaceManager: sends ConfigureMcpBridge vsock message to guest
- Uses `fresh_vsock_client()` pattern (no shared mutex)
- Called during swarm_run when `mcp_bridge=true`: generate token → set_env(AGENTISO_MCP_TOKEN) → ConfigureMcpBridge(bridge_url, token)
- Token lifecycle: generated per-workspace, registered in auth system (maps token → workspace_id), revoked on destroy
- Workers get scoped MCP tool access to only their own workspace

### Configuration (config.toml)
```toml
[storage]
base_image = "alpine-opencode"     # Default base image for workspaces
base_snapshot = "latest"

[vm]
boot_timeout_secs = 10             # Max wait for guest agent readiness (init-fast)

[resources]
default_memory_mb = 2048           # Per-workspace memory
default_vcpus = 2
default_disk_gb = 10
max_workspaces = 20

[pool]
enabled = true
max_size = 4                       # Max warm pool VMs
max_memory_mb = 16384              # Pool memory budget

[mcp_bridge]
enabled = true                     # Enable HTTP MCP transport for VM-based OpenCode
bind_addr = "10.99.0.1"            # Listen on bridge interface
port = 3100                        # MCP bridge port
```

## Build & Test

```bash
cargo test -p agentiso -- workspace  # Workspace tests
cargo test -p agentiso -- pool       # Pool tests
cargo test -p agentiso -- config     # Config tests
cargo test -p agentiso -- snapshot   # Snapshot tests
cargo build --release                # Build host binary
```

## Security Hardening (2026-02-24)

- **cgroup_required config option**: New `cgroup_required = true/false` in config. When true, workspace creation fails if cgroup v2 limits cannot be applied (fail-closed vs best-effort). Prevents resource exhaustion when cgroup setup silently fails.
- **Config validation at startup**: `pool.max_size + max_workspaces <= max_total_vms` checked at config load. Prevents NPROC/resource exhaustion from misconfiguration.
- **MAX_MESSAGE_SIZE reduced to 4 MiB**: vsock protocol MAX_MESSAGE_SIZE reduced from 16 MiB to 4 MiB. Reduces coordinated DoS surface from guest-side message allocation.
- **Snapshot limit (20/workspace)**: Workspace snapshot operations enforce a maximum of 20 snapshots per workspace to prevent ZFS quota bypass via snapshot accumulation (snapshots bypass volsize quotas).
- **TOML plan size pre-check**: Orchestration TOML files are size-checked before parsing to prevent unbounded memory allocation from malicious plan files.

## Key Invariants

1. Pool only dispenses VMs matching the configured base_image
2. set_env uses fresh_vsock_client (never shared mutex)
3. Storage destroy is best-effort (zombie state entries are worse than orphaned zvols)
4. save_state() is serialized via Mutex (prevents temp file write races)
5. Zombie workspaces are cleaned up on load_state()
6. config() accessor provides read-only access to Config for tools.rs
7. MCP bridge tokens are scoped per-workspace and revoked on destroy
8. configure_mcp_bridge uses fresh_vsock_client (same pattern as set_env)
9. cgroup limits MUST be enforced (not best-effort) when `cgroup_required = true`
10. Config validation MUST pass at startup before server accepts connections
11. Snapshots per workspace MUST not exceed 20

## Current Config (OpenCode trial)
- base_image: alpine-opencode (not alpine-dev)
- default_allow_internet: true (needed for API access)
- default_memory_mb: 2048 (OpenCode needs more RAM)
- boot_timeout_secs: 10 (init-fast boot)
- Pool: 4 warm VMs, 16GB budget
- mcp_bridge: enabled, 10.99.0.1:3100
