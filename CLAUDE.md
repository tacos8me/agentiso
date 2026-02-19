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
  - `src/mcp/` — MCP server, tool definitions, auth, vault tools
  - `src/mcp/vault.rs` — VaultManager for Obsidian-style markdown knowledge base
  - `src/vm/` — QEMU process management, QMP client, vsock
  - `src/storage/` — ZFS operations (snapshot, clone, destroy)
  - `src/network/` — TAP/bridge setup, nftables rules, IP allocation
  - `src/workspace/` — Workspace lifecycle state machine, snapshot tree, orchestration
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
- IP allocation is sequential from 10.99.0.0/16 pool

## Host Environment

- **ZFS pool**: `agentiso` on `/mnt/nvme-2` (3TB)
- **Dataset layout**: `agentiso/agentiso/{base,workspaces,forks}`
- **QEMU**: `qemu-system-x86_64` via apt
- **KVM**: `/dev/kvm` available
- **Kernel**: host bzImage at `/var/lib/agentiso/vmlinuz` + initrd at `/var/lib/agentiso/initrd.img`
- **Bridge**: `br-agentiso` at `10.99.0.1/16`

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
# Unit + integration tests (no root needed) — 503 tests
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

**503 tests passing** (450 agentiso + 24 guest-agent + 29 protocol), 4 ignored (integration scaffolding).

**Core platform (complete)**:
- 14 e2e tests, 26/26 MCP integration test steps passing (full tool coverage)
- Guest agent: vsock listener, exec, file ops, process group isolation, hardened (32 MiB limit, hostname/IP validation, exec timeout kill)
- 40 MCP tools with name-or-UUID workspace lookup and contextual error messages
- CLI: `check`, `status`, `logs`, `dashboard` (ratatui TUI)
- Deploy: systemd unit, install script, Claude Code MCP config

**Production hardening (P0-P1 sprint, complete)**:
- Orphan reconciliation on server restart (stale QEMU/TAP/ZFS cleanup)
- Session token persistence across restart
- Per-workspace ZFS volsize quotas with pool space hard-fail
- cgroup v2 memory+CPU limits (best-effort, `agentiso.slice`)
- Parallel VM shutdown via JoinSet (11s worst-case vs N*11s)
- Exec timeout default 30s→120s
- `git_clone` MCP tool with URL validation

**Reliability (Waves 4-5)**:
- Per-QMP-command 10s timeout, exponential backoff on connect
- VM crash detection, console log diagnostics on boot failure
- Vsock reconnect for idempotent operations
- ExecKill protocol + `exec_kill` MCP tool
- `workspace_logs` MCP tool

**Security**:
- Guest: file size limits, hostname/IP validation, exec timeout kill
- VM: HMP tag sanitization, stderr to log file
- MCP/storage: UTF-8 safe truncation, path traversal prevention, dataset hierarchy guard

**OpenCode integration sprint (complete)**:
- SetEnv guest RPC for secure API key injection (env vars via vsock, never on disk)
- Alpine-opencode base image script (musl binary v1.2.6) + rust/python/node toolchain images
- `workspace_prepare` MCP tool: create golden workspace (clone repo, install deps, snapshot)
- `workspace_batch_fork` MCP tool: fork N workers from snapshot in parallel (JoinSet, 1-20)
- OpenCode run wrapper (`vm/opencode.rs`): exec `opencode run` in VM, parse JSON output
- `agentiso orchestrate` CLI: TOML task file → fork workers → inject keys → run OpenCode → collect results
- Prometheus metrics (`/metrics`) + health endpoint (`/healthz`) via `--metrics-port`
- `set_env` MCP tool for secure API key injection into VMs
- 40 MCP tools total

**Vault integration (Phase 1, complete)**:
- 8 native vault MCP tools: `vault_read`, `vault_search`, `vault_list`, `vault_write`, `vault_frontmatter`, `vault_tags`, `vault_replace`, `vault_delete`
- VaultManager in `src/mcp/vault.rs` with path traversal prevention, frontmatter parsing, regex search
- `VaultConfig` in config.toml (`[vault]` section: enabled, path, extensions, exclude_dirs)
- `vault_context` in orchestration TOML: per-task `[[tasks.vault_context]]` with kind="search"/"read"
- Orchestrator resolves vault queries and injects `## Project Knowledge Base` into worker prompts
- Pure Rust implementation (no Node.js, no Obsidian desktop) using `ignore` + `serde_yaml` crates

**Hardening sprint (P0-P3, complete)**:
- 53 new unit tests: guest-agent (24), workspace lifecycle (12), MCP dispatch (15), orchestrate (4)
- Fork concurrency limit (semaphore gates both fork + execution phases)
- Fork error details in TaskResult.stderr (was generic message)
- SIGINT handler in CLI orchestrate (destroys workers before exit)
- Parallel vault_context resolution via JoinSet
- Vault write size limit (10 MiB)
- exec_kill: process group isolation + group kill + wait-for-death
- port_forward: nftables `dnat ip to` fix for inet family

**Known limitations**:
- Graceful VM shutdown may time out; falls back to SIGKILL
- State persistence across restart: not integration-tested (scaffolding in place)

## Design Docs

- `docs/plans/2026-02-16-agentiso-design.md` — Core architecture
- `docs/plans/2026-02-19-opencode-sprint-design.md` — OpenCode integration sprint
- `docs/plans/2026-02-19-vault-integration-design.md` — Obsidian vault integration (Phase 1)
