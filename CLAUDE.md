# agentiso

QEMU microvm workspace manager for AI agents, exposed via MCP tools.

## Tech Stack

- **Language**: Rust (2021 edition)
- **Async**: tokio
- **Serialization**: serde + serde_json
- **MCP**: rmcp crate (Rust MCP SDK) with stdio transport
- **VM**: QEMU `-M microvm` with host kernel (bzImage + initrd)
- **Guest communication**: vsock (virtio-socket), not SSH
- **Storage**: ZFS zvols for workspaces, snapshots, and clones
- **Networking**: TAP devices on `br-agentiso` bridge, nftables for isolation
- **Guest OS**: Alpine Linux with dev tools pre-installed
- **Guest agent**: Separate Rust binary (`agentiso-guest`), statically linked with musl

## Project Structure

- `src/` — Main agentiso binary (MCP server + VM manager)
  - `src/mcp/` — MCP server, tool definitions, auth
  - `src/vm/` — QEMU process management, QMP client, vsock
  - `src/storage/` — ZFS operations (snapshot, clone, destroy)
  - `src/network/` — TAP/bridge setup, nftables rules, IP allocation
  - `src/workspace/` — Workspace lifecycle state machine, snapshot tree
  - `src/guest/` — Guest agent protocol types (re-exports from `agentiso-protocol`)
- `protocol/` — Shared `agentiso-protocol` crate (protocol types used by both host and guest agent)
- `guest-agent/` — Separate crate for the in-VM guest agent binary
- `images/` — Scripts to build Alpine base image and custom kernel
- `tests/` — Unit and integration tests

## Conventions

- Use `anyhow` for error handling in binaries, `thiserror` for library-style error types
- All async code uses tokio runtime
- State is managed in-memory with periodic persistence to a JSON state file
- ZFS operations shell out to `zfs`/`zpool` CLI commands (no libzfs bindings)
- Network operations shell out to `ip` and `nft` commands
- QEMU management via QMP JSON protocol over Unix socket
- Guest agent protocol: length-prefixed (4-byte big-endian) JSON messages over vsock
- Workspace IDs are UUIDs, shortened to first 8 chars for TAP device names and paths
- IP allocation is sequential from 10.42.0.0/16 pool

## Host Environment

- **ZFS pool**: `agentiso` on `/mnt/nvme-2` (3TB)
- **Dataset layout**: `agentiso/agentiso/{base,workspaces,forks}`
- **QEMU**: `qemu-system-x86_64` via apt
- **KVM**: `/dev/kvm` available
- **Kernel**: host bzImage at `/var/lib/agentiso/vmlinuz` + initrd at `/var/lib/agentiso/initrd.img`
- **Bridge**: `br-agentiso` at `10.42.0.1/16`

## Build & Run

```bash
# Build main binary (agentiso-protocol crate builds automatically as a workspace dependency)
cargo build --release

# Build guest agent (musl static; also pulls in agentiso-protocol)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# Setup e2e environment (builds Alpine image, copies kernel, creates ZFS base)
sudo ./scripts/setup-e2e.sh

# Run as MCP server (stdio)
./target/release/agentiso serve

# Run with config (see config.example.toml for all options)
./target/release/agentiso serve --config config.toml
```

## Test

```bash
# Unit + integration tests (no root needed) — 278 tests
cargo test

# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 14 tests
# Requires setup-e2e.sh to have been run first
sudo ./scripts/e2e-test.sh
```

## Swarm Team

This project is built and maintained by a 5-agent swarm. To reactivate for further work, spawn a team named `agentiso-swarm` with these members:

| Agent name | Type | Scope | Files owned |
|------------|------|-------|-------------|
| `guest-agent` | general-purpose | Protocol types, guest binary, image scripts | `protocol/`, `agentiso/src/guest/`, `guest-agent/`, `images/` |
| `vm-engine` | general-purpose | QEMU microvm, QMP, vsock | `agentiso/src/vm/` |
| `storage-net` | general-purpose | ZFS storage, TAP/bridge, nftables | `agentiso/src/storage/`, `agentiso/src/network/` |
| `workspace-core` | general-purpose | Lifecycle orchestration, config, main | `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs` |
| `mcp-server` | general-purpose | MCP server, tools, auth | `agentiso/src/mcp/` |

Dependency chain: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

See `AGENTS.md` for full role descriptions and shared interfaces.

## Current Status

**All tests passing (DONE)**:
- 278 unit tests passing, 0 warnings
- 14 e2e tests passing: ZFS clones, TAP networking, QEMU microvm boot, QMP protocol, vsock guest agent Ping/Pong, ZFS snapshots/forks, QMP shutdown
- 14/14 MCP integration test steps passing (`scripts/test-mcp-integration.sh`): full lifecycle — create workspace → exec → file_write → file_read → snapshot → workspace_info → workspace_ip → destroy
- Guest agent binary: vsock listener with AsyncFd<OwnedFd>, length-prefixed JSON protocol, exec/file ops, hardened with file size limits (32 MiB), hostname/IP validation, and exec timeout kill
- Shared `agentiso-protocol` crate: protocol types extracted into `protocol/`, consumed by both host and guest agent (eliminates duplicated types and prevents stale binary bugs)
- Host environment: ZFS pool, bridge, kernel+initrd, Alpine base image with dev tools
- CLI: `agentiso check` (12-prerequisite checker), `agentiso status` (workspace table with PID liveness check), and `agentiso logs <id>` (QEMU console/stderr log viewer)
- Deploy: systemd unit, install script, Claude Code MCP config in `deploy/`

**Reliability and VM health (Wave 4)**:
- Per-QMP-command timeout (10s) and exponential backoff on QMP connect retries
- VM crash detection via `VmManager::check_vm_alive()` (checks if QEMU process exited)
- Console log diagnostics on boot failure (last 30 lines of console.log and qemu-stderr.log in error context)
- Vsock reconnect for idempotent operations (ping, configure_*, file_read, list_dir) on transient connection failures

**Protocol and developer experience (Wave 5)**:
- ExecKill protocol variant and `exec_kill` MCP tool for killing background jobs by job_id with configurable signal
- `workspace_logs` MCP tool for retrieving QEMU console.log and qemu-stderr.log for debugging
- `configure_network` retry in guest agent (retries `ip route add default` once on transient failure)
- 28 MCP tools total

**Security hardening**:
- Guest agent: file size limit (32 MiB) on reads/downloads, hostname validation (RFC 1123), IP address validation, exec timeout kills child process
- VM engine: HMP tag sanitization in QMP savevm/loadvm/delvm prevents command injection, QEMU stderr redirected to log file prevents QEMU hang
- MCP/storage: UTF-8 safe output truncation, base_image path traversal prevention, destroy() safety guard on dataset hierarchy
- Workspace lifecycle: vsock CID recycled on create/fork rollback, save_state() failures logged as warnings

**Bug fixes**:
- ZFS `refquota` removed from zvol clones — `refquota` is a filesystem-only ZFS property, invalid for zvols (block devices). Zvols inherit `volsize` from parent snapshot.

**Known limitations**:
- No TUI — CLI is plain-text output only (`status`, `check`, `logs`). Primary interface is MCP tools.
- State persistence across server restart: not integration-tested yet
- Port forwarding and network policy: not integration-tested yet

## Design Doc

See `docs/plans/2026-02-16-agentiso-design.md` for full architecture and design decisions.
