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
  - `src/mcp/` — MCP server, tool definitions, auth, vault tools, team tools
  - `src/mcp/vault.rs` — VaultManager for Obsidian-style markdown knowledge base
  - `src/mcp/team_tools.rs` — Team MCP tool handler (create/destroy/status/list)
  - `src/mcp/git_tools.rs` — Git MCP tool handlers (clone, status, commit, push, diff)
  - `src/team/` — Team lifecycle (TeamManager, AgentCard, RoleDef)
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

# One-shot environment setup (ZFS pool, bridge, kernel, config, verification)
sudo ./target/release/agentiso init

# Or manual setup (equivalent to agentiso init)
sudo ./scripts/setup-e2e.sh

# Run as MCP server (stdio)
./target/release/agentiso serve

# Run with config (see config.example.toml for all options)
./target/release/agentiso serve --config config.toml
```

## Test

```bash
# Unit + integration tests (no root needed) — 713 tests
cargo test

# E2E test (needs root for QEMU/KVM/TAP/ZFS) — 51 steps
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

**646 unit tests passing**, 4 ignored, 0 warnings.

**Core platform (complete)**:
- 51/51 MCP integration test steps passing (full tool coverage including team lifecycle + task board)
- 10/10 state persistence tests passing
- Guest agent: vsock listener, exec, file ops, process group isolation, hardened (32 MiB limit, hostname/IP validation, exec timeout kill, ENV/BASH_ENV blocklist, output truncation)
- 28 MCP tools with name-or-UUID workspace lookup and contextual error messages
- CLI: `check`, `status`, `logs`, `dashboard` (ratatui TUI)
- Deploy: systemd unit, install script, OpenCode MCP config

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
- ExecKill protocol + `exec_background(action="kill")` MCP tool
- `workspace_logs` MCP tool
- DNS reconfiguration on `network_policy` toggle — guest DNS updated via vsock `ConfigureNetwork` message when internet access is enabled/disabled

**Security**:
- Guest: file size limits, hostname/IP validation, exec timeout kill
- VM: HMP tag sanitization, stderr to log file
- MCP/storage: UTF-8 safe truncation, path traversal prevention, dataset hierarchy guard
- Token-bucket rate limiting (create 5/min, exec 60/min, default 120/min)
- ZFS volsize enforcement on workspace create/fork with atomic quota check
- Per-interface ip_forward (scoped to br-agentiso, not global)
- Init.rs security: SUDO_USER validation, shell escaping, secure tempfile
- Credential redaction in git_push
- PID reuse verification in auto-adopt
- Internet access disabled by default (`default_allow_internet = false`)
- `network_policy` reconfigures guest DNS via vsock when toggling internet access (prevents stale `/etc/resolv.conf`)

**OpenCode integration sprint (complete)**:
- SetEnv guest RPC for secure API key injection (env vars via vsock, never on disk)
- Alpine-opencode base image script (musl binary v1.2.6) + rust/python/node toolchain images
- `workspace_prepare` MCP tool: create golden workspace (clone repo, install deps, snapshot)
- `workspace_fork` with `count` param: fork N workers from snapshot in parallel (JoinSet, 1-20)
- OpenCode run wrapper (`vm/opencode.rs`): exec `opencode run` in VM, parse JSON output
- `agentiso orchestrate` CLI: TOML task file → fork workers → inject keys → run OpenCode → collect results
- Prometheus metrics (`/metrics`) + health endpoint (`/healthz`) via `--metrics-port`
- `set_env` MCP tool for secure API key injection into VMs
- 28 MCP tools total (snapshot, vault, exec_background, port_forward, workspace_fork, file_transfer, workspace_adopt, team bundled; git tools added)

**Vault integration (Phase 1, complete)**:
- 1 bundled `vault` MCP tool with 11 sub-actions: read, search, list, write, frontmatter, tags, replace, delete, move, batch_read, stats
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
- Instance lock for `agentiso orchestrate` CLI (prevents concurrent runs)
- Parallel vault_context resolution via JoinSet
- Vault write size limit (10 MiB)
- exec_background kill action: process group isolation + group kill + wait-for-death
- port_forward: nftables `dnat ip to` fix for inet family
- Guest agent security hardening: ENV/BASH_ENV blocklist, output truncation
- State persistence fully tested (10/10 integration tests)

**Runtime defaults**:
- Warm pool enabled by default (size=2), auto-replenish on claim
- Auto-adopt running workspaces on server restart
- Internet access disabled by default (`default_allow_internet = false`, secure by default)
- Rate limiting enabled by default (`[rate_limit]` section in config)

**Known limitations**:
- Graceful VM shutdown may time out; falls back to SIGKILL

**Multi-agent teams (Phases 2-3, complete)**:
- `team` MCP tool with 4 actions: create, destroy, status, list
- TeamManager: create teams with named roles, each getting its own workspace VM
- AgentCard (A2A-style): stored as JSON in vault at `teams/{name}/cards/{member}.json`
- `team_id` field on Workspace (persisted in state v3) for team membership tracking
- Intra-team nftables rules: team members can communicate, non-members are isolated
- Parallel workspace teardown on team destroy via JoinSet
- Global fork semaphore for concurrency control across team + regular fork operations
- **TaskBoard**: vault-backed task board with YAML frontmatter markdown files
  - Full lifecycle: create, claim, start, complete, fail, release
  - Dependency resolution: `available_tasks`, `is_task_ready`, `dependency_order` (Kahn's topological sort with cycle detection)
  - Auto-generated INDEX.md on every task state change (grouped by status with markers)
  - TaskClaim protocol message for atomic claiming via vsock
- 51/51 MCP integration test steps (5 team lifecycle + 6 task board steps)
- 713 unit tests (team: 10 TeamManager + 5 AgentCard + 8 team_tools + 40 TaskBoard)

## Design Docs

- `docs/plans/2026-02-16-agentiso-design.md` — Core architecture
- `docs/plans/2026-02-19-opencode-sprint-design.md` — OpenCode integration sprint
- `docs/plans/2026-02-19-vault-integration-design.md` — Obsidian vault integration (Phase 1)
- `docs/plans/2026-02-19-teams-design.md` — Multi-agent team coordination (Phases 2-3, complete)
- `docs/plans/2026-02-19-teams-impl-plan.md` — Teams implementation plan (41 tasks, 6 phases)
