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
  - `src/guest/` — Guest agent protocol types
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
# Build main binary
cargo build --release

# Build guest agent (musl static)
cargo build --release --target x86_64-unknown-linux-musl -p agentiso-guest

# Setup e2e environment (builds Alpine image, copies kernel, creates ZFS base)
sudo ./scripts/setup-e2e.sh

# Run as MCP server (stdio)
./target/release/agentiso serve

# Run with config
./target/release/agentiso serve --config config.toml
```

## Test

```bash
# Unit + integration tests (no root needed) — 172 tests
cargo test

# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 14 tests
# Requires setup-e2e.sh to have been run first
sudo ./scripts/e2e-test.sh
```

## Swarm Team

This project is built and maintained by a 5-agent swarm. To reactivate for further work, spawn a team named `agentiso-swarm` with these members:

| Agent name | Type | Scope | Files owned |
|------------|------|-------|-------------|
| `guest-agent` | general-purpose | Protocol types, guest binary, image scripts | `agentiso/src/guest/`, `guest-agent/`, `images/` |
| `vm-engine` | general-purpose | QEMU microvm, QMP, vsock | `agentiso/src/vm/` |
| `storage-net` | general-purpose | ZFS storage, TAP/bridge, nftables | `agentiso/src/storage/`, `agentiso/src/network/` |
| `workspace-core` | general-purpose | Lifecycle orchestration, config, main | `agentiso/src/workspace/`, `agentiso/src/config.rs`, `agentiso/src/main.rs` |
| `mcp-server` | general-purpose | MCP server, tools, auth | `agentiso/src/mcp/` |

Dependency chain: `guest-agent` -> `vm-engine` + `storage-net` (parallel) -> `workspace-core` -> `mcp-server`

See `AGENTS.md` for full role descriptions and shared interfaces.

## Current Status

**All tests passing (DONE)**:
- 194 unit tests passing, 0 warnings
- 14 e2e tests passing: ZFS clones, TAP networking, QEMU microvm boot, QMP protocol, vsock guest agent Ping/Pong, ZFS snapshots/forks, QMP shutdown
- 14/14 MCP integration test steps passing (`scripts/test-mcp-integration.sh`): full lifecycle — create workspace → exec → file_write → file_read → snapshot → workspace_info → workspace_ip → destroy
- Guest agent binary: vsock listener with AsyncFd<OwnedFd>, length-prefixed JSON protocol, exec/file ops
- Host environment: ZFS pool, bridge, kernel+initrd, Alpine base image with dev tools
- CLI: `agentiso check` (12-prerequisite checker) and `agentiso status` (workspace table)
- Deploy: systemd unit, install script, Claude Code MCP config in `deploy/`

**Known limitations**:
- Graceful VM shutdown: microvm ACPI powerdown may time out; currently falls back to SIGKILL
- State persistence across server restart: not integration-tested yet
- Port forwarding and network policy: not integration-tested yet

## Design Doc

See `docs/plans/2026-02-16-agentiso-design.md` for full architecture and design decisions.
